//! One run of one module: its process, the protocol spoken over its pipes,
//! and how the run ended.
//!
//! Four tasks serve a run. The run task owns the process and the protocol;
//! a writer task drains a bounded queue into the module's stdin, so a module
//! that stops reading fills the queue and is killed rather than blocking
//! anything; a reader task frames its stdout into messages, so a long line
//! never sits half-read in a `select!`; and a stderr task copies its stderr
//! into the agent's log at debug level.
//!
//! On Unix a module leads its own process group, and a kill takes the whole
//! group: a wrapper script's children go with it instead of holding its
//! pipes open. Windows kills the module's own process only.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use openlogi_core::binding::GridSlot;
use openlogi_core::module::{ENV_MODULE_DIR, ENV_MODULE_ID, ModuleId, PROTOCOL};
use tokio::io::{AsyncBufRead, AsyncRead, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

use super::protocol::{self, Incoming, LineRead, LogLevel};
use super::{ModuleEvent, ModuleKey, ModuleTimings};

/// How many host lines may wait for a module to read them. A module that
/// lets this many pile up has stopped reading and is killed.
const WRITE_QUEUE: usize = 64;
/// How many presses may wait for a module's run task.
const PRESS_QUEUE: usize = 16;
/// How many framed stdout messages may wait for the run task: beyond it the
/// reader stops reading and the module's own writes block.
const READ_QUEUE: usize = 8;
/// The longest stderr line kept; the rest of it is cut.
const MAX_STDERR_LINE: usize = 4 * 1024;

/// What a run needs to start.
#[derive(Debug, Clone)]
pub(super) struct RunSpec {
    pub(super) id: ModuleId,
    pub(super) folder: PathBuf,
    pub(super) executable: PathBuf,
    pub(super) settings: toml::Table,
    pub(super) timings: ModuleTimings,
}

/// What a run reports to the host, tagged with the run's generation.
#[derive(Debug)]
pub(super) enum RunEvent {
    /// The module answered `hello`.
    Ready,
    /// The module asked for a face change.
    Face(ModuleEvent),
    /// The run is over and its process gone.
    Ended(RunEnd),
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RunEnd {
    /// How long the module ran after `ready`; `None` when it never sent it.
    pub(super) ready_for: Option<Duration>,
    /// Why the run ended, when the host did not end it.
    pub(super) error: Option<String>,
}

/// Where a run reports, and as which generation.
#[derive(Clone)]
pub(super) struct EventSink {
    pub(super) id: ModuleId,
    pub(super) generation: u64,
    pub(super) events: mpsc::Sender<(ModuleId, u64, RunEvent)>,
}

impl EventSink {
    async fn send(&self, event: RunEvent) {
        if self
            .events
            .send((self.id.clone(), self.generation, event))
            .await
            .is_err()
        {
            debug!(module = %self.id, "module host gone: run event dropped");
        }
    }
}

/// The host's hold on a live run.
pub(super) struct RunHandle {
    /// The keys the module owns; the run sends `keys` whenever they change.
    pub(super) keys: watch::Sender<Arc<Vec<ModuleKey>>>,
    presses: mpsc::Sender<(String, GridSlot)>,
    stop: Option<oneshot::Sender<()>>,
}

impl RunHandle {
    /// Hand the module a press. Dropped when the run is too far behind.
    pub(super) fn press(&self, device: String, slot: GridSlot) -> bool {
        self.presses.try_send((device, slot)).is_ok()
    }

    /// Tell the module to shut down; the run kills it after its grace.
    pub(super) fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// Start one run of `spec`, owning `keys` until told otherwise.
pub(super) fn start(spec: RunSpec, keys: Arc<Vec<ModuleKey>>, sink: EventSink) -> RunHandle {
    let (keys_tx, keys_rx) = watch::channel(keys);
    let (presses, press_rx) = mpsc::channel(PRESS_QUEUE);
    let (stop, stop_rx) = oneshot::channel();
    tokio::spawn(async move {
        let end = Run {
            spec,
            sink: sink.clone(),
            keys: keys_rx,
            presses: press_rx,
            stop: stop_rx,
            log: LogBudget::default(),
            unknown: HashSet::new(),
        }
        .drive()
        .await;
        sink.send(RunEvent::Ended(end)).await;
    });
    RunHandle {
        keys: keys_tx,
        presses,
        stop: Some(stop),
    }
}

/// A run in progress.
struct Run {
    spec: RunSpec,
    sink: EventSink,
    keys: watch::Receiver<Arc<Vec<ModuleKey>>>,
    presses: mpsc::Receiver<(String, GridSlot)>,
    stop: oneshot::Receiver<()>,
    log: LogBudget,
    /// The unknown message types already logged, each logged once.
    unknown: HashSet<String>,
}

/// One framed stdout item, from the reader task.
enum Read {
    Message(Incoming),
    Malformed(String),
    TooLong,
    End(Option<std::io::Error>),
}

/// What handling one message asks of the run.
enum Next {
    Continue,
    /// Kill the module, for this reason.
    End(String),
}

impl Run {
    async fn drive(mut self) -> RunEnd {
        let id = self.spec.id.clone();
        let mut command = Command::new(&self.spec.executable);
        // No arguments; the agent's environment plus the module's id and
        // folder.
        command
            .current_dir(&self.spec.folder)
            .env(ENV_MODULE_ID, id.as_ref())
            .env(ENV_MODULE_DIR, &self.spec.folder)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        // A console module started by the windowless agent would otherwise
        // open a console window of its own.
        #[cfg(windows)]
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return RunEnd {
                    ready_for: None,
                    error: Some(format!("could not start: {error}")),
                };
            }
        };
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return kill(
                &mut child,
                None,
                Some("its pipes could not be opened".into()),
            )
            .await;
        };
        info!(module = %id, pid = ?child.id(), "module started");
        let writer = spawn_writer(stdin);
        let mut reads = spawn_reader(BufReader::new(stdout));
        tokio::spawn(copy_stderr(BufReader::new(stderr), id.clone()));

        let _ = writer.try_send(protocol::hello(&id, &self.spec.settings));
        let ready_deadline = Instant::now() + self.spec.timings.ready_timeout;
        let mut ready_at: Option<Instant> = None;
        loop {
            let is_ready = ready_at.is_some();
            let next = tokio::select! {
                _ = &mut self.stop => {
                    return shut_down(&id, &mut child, writer, ready_at, self.spec.timings.shutdown_grace)
                        .await;
                }
                () = tokio::time::sleep_until(ready_deadline), if !is_ready => {
                    let timeout = self.spec.timings.ready_timeout;
                    let error = format!("did not answer hello within {} s", timeout.as_secs_f32());
                    return kill(&mut child, None, Some(error)).await;
                }
                read = reads.recv() => match read {
                    Some(Read::Message(message)) => self.handle(message, &mut ready_at).await,
                    Some(Read::Malformed(error)) => {
                        self.log.warn(&id, format_args!("dropped a line: {error}"));
                        Next::Continue
                    }
                    Some(Read::TooLong) => {
                        self.log.warn(
                            &id,
                            format_args!("dropped a line longer than {} MiB", protocol::MAX_LINE >> 20),
                        );
                        Next::Continue
                    }
                    end @ (Some(Read::End(_)) | None) => {
                        if let Some(Read::End(Some(error))) = end {
                            debug!(module = %id, %error, "module stdout failed");
                        }
                        return exited(&mut child, ready_at, self.spec.timings.shutdown_grace).await;
                    }
                },
                changed = self.keys.changed(), if is_ready => {
                    if changed.is_err() {
                        return shut_down(&id, &mut child, writer, ready_at, self.spec.timings.shutdown_grace)
                            .await;
                    }
                    let keys = Arc::clone(&self.keys.borrow_and_update());
                    queue(&writer, protocol::keys(&keys))
                }
                Some((device, slot)) = self.presses.recv(), if is_ready => {
                    queue(&writer, protocol::pressed(&device, slot))
                }
            };
            if let Next::End(error) = next {
                return kill(&mut child, ready_at.map(|at| at.elapsed()), Some(error)).await;
            }
            if !is_ready && let Some(ready_at) = ready_at {
                debug!(module = %id, "module ready");
                let keys = Arc::clone(&self.keys.borrow_and_update());
                if let Next::End(error) = queue(&writer, protocol::keys(&keys)) {
                    return kill(&mut child, Some(ready_at.elapsed()), Some(error)).await;
                }
            }
        }
    }

    /// Act on one message from the module.
    async fn handle(&mut self, message: Incoming, ready_at: &mut Option<Instant>) -> Next {
        let id = self.spec.id.clone();
        match message {
            Incoming::Ready { protocol } if ready_at.is_none() => {
                if protocol != PROTOCOL {
                    return Next::End(format!(
                        "answered with protocol {protocol}; this OpenLogi speaks {PROTOCOL}"
                    ));
                }
                *ready_at = Some(Instant::now());
                self.sink.send(RunEvent::Ready).await;
            }
            Incoming::Ready { .. } => debug!(module = %id, "module sent ready again: ignored"),
            Incoming::Log { level, message } => self.log.module(&id, level, &message),
            Incoming::Unknown(kind) => {
                if self.unknown.insert(kind.clone()) {
                    info!(module = %id, kind, "module sent a message type this OpenLogi does not know: ignored");
                }
            }
            // Only `log` may come before `ready`; anything else is a protocol
            // error, logged and dropped.
            _ if ready_at.is_none() => {
                self.log.warn(
                    &id,
                    format_args!("sent a message other than log before ready: dropped"),
                );
            }
            Incoming::Image { device, slot, data } => {
                if !self.owns(&device, slot) {
                    debug!(module = %id, device, slot = %slot.config_name(), "image for a key the module does not own: ignored");
                    return Next::Continue;
                }
                if let Some(image) = self.decode(data).await {
                    self.sink
                        .send(RunEvent::Face(ModuleEvent::Image {
                            module: id,
                            device,
                            slot,
                            image,
                        }))
                        .await;
                }
            }
            Incoming::ImageAll { data } => {
                if self.keys.borrow().is_empty() {
                    debug!(module = %id, "image for all keys while the module owns none: ignored");
                    return Next::Continue;
                }
                if let Some(image) = self.decode(data).await {
                    self.sink
                        .send(RunEvent::Face(ModuleEvent::ImageAll { module: id, image }))
                        .await;
                }
            }
            Incoming::Clear { device, slot } => {
                if self.owns(&device, slot) {
                    self.sink
                        .send(RunEvent::Face(ModuleEvent::Clear {
                            module: id,
                            device,
                            slot,
                        }))
                        .await;
                }
            }
        }
        Next::Continue
    }

    /// Whether the module owns `slot` of `device` now.
    fn owns(&self, device: &str, slot: GridSlot) -> bool {
        self.keys
            .borrow()
            .iter()
            .any(|key| key.device == device && key.slot == slot)
    }

    /// Decode an image on the blocking pool, one at a time: the run reads
    /// nothing more from the module meanwhile.
    async fn decode(&mut self, data: String) -> Option<crate::watchers::key_display::KeyImage> {
        match tokio::task::spawn_blocking(move || protocol::decode_image(&data)).await {
            Ok(Ok(image)) => Some(image),
            Ok(Err(error)) => {
                self.log
                    .warn(&self.spec.id, format_args!("dropped an image: {error}"));
                None
            }
            Err(error) => {
                self.log
                    .warn(&self.spec.id, format_args!("dropped an image: {error}"));
                None
            }
        }
    }
}

/// Queue one line for the module, or end the run when the module has
/// stopped reading.
fn queue(writer: &mpsc::Sender<String>, line: String) -> Next {
    match writer.try_send(line) {
        Err(mpsc::error::TrySendError::Full(_)) => Next::End("stopped reading its input".into()),
        // A closed writer means the module closed its stdin or exited, and
        // the reader will see its stdout end.
        Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => Next::Continue,
    }
}

/// Drain queued lines into `stdin` until the queue closes or a write fails.
fn spawn_writer(mut stdin: ChildStdin) -> mpsc::Sender<String> {
    let (writer, mut lines) = mpsc::channel::<String>(WRITE_QUEUE);
    tokio::spawn(async move {
        while let Some(line) = lines.recv().await {
            if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                return;
            }
        }
    });
    writer
}

/// Frame `stdout` into messages until it ends.
fn spawn_reader<R: AsyncBufRead + Unpin + Send + 'static>(mut stdout: R) -> mpsc::Receiver<Read> {
    let (reads, read_rx) = mpsc::channel(READ_QUEUE);
    tokio::spawn(async move {
        let mut line = Vec::new();
        loop {
            let read = match protocol::read_line(&mut stdout, &mut line, protocol::MAX_LINE).await {
                Ok(LineRead::Line) if line.iter().all(u8::is_ascii_whitespace) => continue,
                Ok(LineRead::Line) => match protocol::parse(&line) {
                    Ok(message) => Read::Message(message),
                    Err(error) => Read::Malformed(error.to_string()),
                },
                Ok(LineRead::TooLong) => Read::TooLong,
                Ok(LineRead::Eof) => Read::End(None),
                Err(error) => Read::End(Some(error)),
            };
            let end = matches!(read, Read::End(_));
            if reads.send(read).await.is_err() || end {
                return;
            }
        }
    });
    read_rx
}

/// Copy the module's stderr into the agent's log at debug level, a line at
/// a time, within the same budget as its other output.
async fn copy_stderr<R: AsyncRead + Unpin>(stderr: BufReader<R>, id: ModuleId) {
    let mut stderr = stderr;
    let mut line = Vec::new();
    let mut budget = LogBudget::default();
    loop {
        match protocol::read_line(&mut stderr, &mut line, MAX_STDERR_LINE).await {
            Ok(LineRead::Line) => {
                let text = String::from_utf8_lossy(&line);
                budget.emit(
                    Instant::now(),
                    &id,
                    |id| debug!(module = %id, "stderr: {text}"),
                );
            }
            Ok(LineRead::TooLong) => {
                budget.emit(Instant::now(), &id, |id| {
                    debug!(module = %id, "stderr: a line longer than {MAX_STDERR_LINE} bytes, dropped");
                });
            }
            Ok(LineRead::Eof) | Err(_) => return,
        }
    }
}

/// The module's stdout ended: give the process its grace to exit, kill it
/// past that, and report how it ended.
async fn exited(child: &mut Child, ready_at: Option<Instant>, grace: Duration) -> RunEnd {
    let ready_for = ready_at.map(|at| at.elapsed());
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(Ok(status)) => RunEnd {
            ready_for,
            error: Some(describe(status)),
        },
        Ok(Err(error)) => RunEnd {
            ready_for,
            error: Some(format!("could not be waited on: {error}")),
        },
        Err(_) => {
            kill(
                child,
                ready_for,
                Some("closed its output and kept running".into()),
            )
            .await
        }
    }
}

/// The host asked the module `id` to leave: send `shutdown`, close its
/// stdin, and kill it if it is still there after `grace`.
async fn shut_down(
    id: &ModuleId,
    child: &mut Child,
    writer: mpsc::Sender<String>,
    ready_at: Option<Instant>,
    grace: Duration,
) -> RunEnd {
    let ready_for = ready_at.map(|at| at.elapsed());
    let _ = writer.try_send(protocol::shutdown());
    drop(writer);
    if tokio::time::timeout(grace, child.wait()).await.is_err() {
        kill_group(child);
        let _ = child.wait().await;
        info!(module = %id, "module did not shut down in time: killed");
    }
    RunEnd {
        ready_for,
        error: None,
    }
}

/// Kill the process and report the run as `ready_for` and `error`.
async fn kill(child: &mut Child, ready_for: Option<Duration>, error: Option<String>) -> RunEnd {
    kill_group(child);
    let _ = child.wait().await;
    RunEnd { ready_for, error }
}

/// Kill the module and, on Unix, every process left in its group. Called
/// before the module is reaped: its group, which bears its id, cannot be
/// another's until then.
fn kill_group(child: &mut Child) {
    #[cfg(unix)]
    if let Some(group) = child.id().and_then(|pid| libc::pid_t::try_from(pid).ok()) {
        #[expect(
            unsafe_code,
            reason = "killpg is the one call that signals a whole process group"
        )]
        // SAFETY: killpg only sends a signal; `group` is the group the
        // module leads (`process_group(0)`), and the unreaped module keeps
        // that id from being reused.
        unsafe {
            libc::killpg(group, libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
}

fn describe(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exited with status {code}"),
        None => "was ended by a signal".into(),
    }
}

/// A run's allowance of log lines: so many per window, and one note of how
/// many were dropped once the window turns.
#[derive(Debug, Default)]
struct LogBudget {
    window_start: Option<Instant>,
    used: u32,
    dropped: u32,
}

/// Log lines a run may write per [`LOG_WINDOW`].
const LOG_LINES: u32 = 50;
/// The window [`LOG_LINES`] counts over.
const LOG_WINDOW: Duration = Duration::from_secs(10);

impl LogBudget {
    /// Whether a line may be logged at `now`, and how many were dropped
    /// since the last one that was.
    fn allow(&mut self, now: Instant) -> Option<u32> {
        let fresh = self
            .window_start
            .is_none_or(|start| now.duration_since(start) >= LOG_WINDOW);
        if fresh {
            self.window_start = Some(now);
            self.used = 0;
        }
        if self.used >= LOG_LINES {
            self.dropped += 1;
            return None;
        }
        self.used += 1;
        Some(std::mem::take(&mut self.dropped))
    }

    fn emit(&mut self, now: Instant, id: &ModuleId, log: impl FnOnce(&ModuleId)) {
        if let Some(dropped) = self.allow(now) {
            if dropped > 0 {
                info!(module = %id, dropped, "module output was rate-limited");
            }
            log(id);
        }
    }

    fn warn(&mut self, id: &ModuleId, message: std::fmt::Arguments<'_>) {
        self.emit(Instant::now(), id, |id| warn!(module = %id, "{message}"));
    }

    fn module(&mut self, id: &ModuleId, level: LogLevel, message: &str) {
        self.emit(Instant::now(), id, |id| match level {
            LogLevel::Debug => debug!(module = %id, "{message}"),
            LogLevel::Info => info!(module = %id, "{message}"),
            LogLevel::Warn => warn!(module = %id, "{message}"),
            LogLevel::Error => error!(module = %id, "{message}"),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_budget_holds_a_window_and_counts_what_it_dropped() {
        let start = Instant::now();
        let mut budget = LogBudget::default();
        for _ in 0..LOG_LINES {
            assert_eq!(budget.allow(start), Some(0));
        }
        assert_eq!(budget.allow(start), None);
        assert_eq!(budget.allow(start + Duration::from_secs(9)), None);
        assert_eq!(
            budget.allow(start + LOG_WINDOW),
            Some(2),
            "a new window reports the two dropped lines"
        );
        assert_eq!(budget.allow(start + LOG_WINDOW), Some(0));
    }
}
