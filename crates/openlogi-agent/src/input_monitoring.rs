//! Reading Input Monitoring the way a newly started agent would, and
//! relaunching to use a grant this process cannot.
//!
//! macOS answers a running process's Input Monitoring check from what that
//! process saw when it first asked, so a grant made after the agent's launch
//! prompt never reaches it (#1304). The watcher in
//! `openlogi_agent_core::watchers::input_monitoring` therefore asks a probe:
//! a throwaway instance of this executable that answers before any agent
//! machinery starts. The probe launches exactly the way
//! [`crate::binary_watch::schedule`] launches a successor — through
//! LaunchServices for the packaged helper, directly for a bare dev binary — so
//! it carries the successor's TCC identity, and "the probe holds the grant"
//! means "a relaunched agent will". Karabiner-Elements' permission checker
//! makes the same probe-then-restart move.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tracing::{debug, warn};

/// The argument that makes this executable a probe. The probe's only other
/// argument is the path it writes its answer to.
const PROBE_ARG: &str = "--probe-input-monitoring";

/// How long a probe may take to answer. A LaunchServices launch answers in
/// well under a second; the first launch after an install also waits for the
/// system's code assessment.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the result file is checked while a probe runs.
const PROBE_POLL: Duration = Duration::from_millis(50);

/// The name prefix of every probe answer file in the runtime dir, staged
/// copies included.
const ANSWER_PREFIX: &str = "input-monitoring-probe.";

/// The shortest time between two relaunches for a late grant. A relaunched
/// agent that still lacks the grant its probe saw would otherwise relaunch
/// again on its first sample, forever.
const RELAUNCH_COOLDOWN: Duration = Duration::from_secs(60);

/// What a probe read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    Granted,
    Missing,
}

impl Answer {
    fn encode(self) -> &'static str {
        match self {
            Self::Granted => "granted\n",
            Self::Missing => "missing\n",
        }
    }

    fn decode(text: &str) -> Option<Self> {
        match text.trim_end() {
            "granted" => Some(Self::Granted),
            "missing" => Some(Self::Missing),
            _ => None,
        }
    }
}

/// Answer a probe request when this process was started as one, and report
/// whether it was. `main` calls this before anything else: a probe must not
/// write the agent's log, take its singleton lock, or open a device.
pub(crate) fn answer_probe_if_asked() -> bool {
    let Some(result) = probe_result_path(std::env::args_os().skip(1)) else {
        return false;
    };
    let answer = if openlogi_hid::permissions::has_access() {
        Answer::Granted
    } else {
        Answer::Missing
    };
    // Nothing to report a failure to: the agent that asked times out and
    // logs it.
    let _ = write_atomically(&result, answer.encode());
    true
}

/// The result path of a probe request: exactly [`PROBE_ARG`] and one path.
fn probe_result_path(mut args: impl Iterator<Item = OsString>) -> Option<PathBuf> {
    let (Some(flag), Some(path), None) = (args.next(), args.next(), args.next()) else {
        return None;
    };
    (flag == PROBE_ARG).then(|| PathBuf::from(path))
}

/// Written beside `path` and renamed into place, so the agent never reads a
/// partial answer.
fn write_atomically(path: &Path, contents: &str) -> io::Result<()> {
    let staged = staged_path(path);
    std::fs::write(&staged, contents)?;
    std::fs::rename(&staged, path)
}

/// `path` with `.tmp` appended. Appended, not swapped for the extension:
/// answer names end in an attempt number, and two attempts must not share a
/// staged file.
fn staged_path(path: &Path) -> PathBuf {
    let mut staged = path.as_os_str().to_owned();
    staged.push(".tmp");
    PathBuf::from(staged)
}

/// Read the Input Monitoring grant from a newly started probe: `Some(granted)`,
/// or `None` when no probe answered within [`PROBE_TIMEOUT`].
pub(crate) fn fresh_grant() -> Option<bool> {
    match probe() {
        Ok(answer) => {
            debug!(?answer, "Input Monitoring probe answered");
            Some(answer == Answer::Granted)
        }
        Err(error) => {
            // The watcher keeps probing while the grant is missing; one
            // warning says the probe is broken, the rest are noise.
            static WARNED: AtomicBool = AtomicBool::new(false);
            if WARNED.swap(true, Ordering::Relaxed) {
                debug!(%error, "Input Monitoring probe failed");
            } else {
                warn!(%error, "Input Monitoring probe failed — a grant made after launch needs a manual agent restart");
            }
            None
        }
    }
}

fn probe() -> io::Result<Answer> {
    let exe = std::env::current_exe()?;
    let dir = openlogi_core::paths::runtime_dir().map_err(io::Error::other)?;
    std::fs::create_dir_all(&dir)?;
    // A packaged probe runs under LaunchServices, out of reach of `reap`, so
    // one that misses its deadline can still answer later. Each attempt
    // therefore gets its own file, so a late answer is never read as a later
    // attempt's, and the sweep removes such leftovers.
    sweep_answers(&dir);
    let result = dir.join(answer_name(std::process::id(), next_attempt()));
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let child = probe_command(&exe, &result).spawn()?;
    let answer = wait_for_answer(&result, deadline);
    reap(child, deadline);
    let _ = remove_if_present(&result);
    answer
}

/// The answer file of one probe attempt.
fn answer_name(pid: u32, attempt: u64) -> String {
    format!("{ANSWER_PREFIX}{pid}.{attempt}")
}

/// A number no earlier attempt in this process used.
fn next_attempt() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Remove every answer file left in `dir`, from this agent or an earlier
/// one. The singleton lock leaves no other asker whose answer this could
/// take; a failure only leaves a stray file for the next sweep.
fn sweep_answers(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name
            .to_str()
            .is_some_and(|name| name.starts_with(ANSWER_PREFIX))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The probe launch, mirroring [`crate::binary_watch::schedule`]'s successor.
fn probe_command(exe: &Path, result: &Path) -> Command {
    let mut command = if let Some(bundle) = openlogi_core::brand::helper_bundle_root(exe) {
        let mut open = Command::new("/usr/bin/open");
        open.args([OsStr::new("-g"), OsStr::new("-n")])
            .arg(bundle)
            .arg("--args");
        open
    } else {
        Command::new(exe)
    };
    command
        .arg(PROBE_ARG)
        .arg(result)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn wait_for_answer(result: &Path, deadline: Instant) -> io::Result<Answer> {
    loop {
        match std::fs::read_to_string(result) {
            Ok(text) => {
                return Answer::decode(&text).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unreadable answer {text:?}"),
                    )
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "no answer"));
        }
        std::thread::sleep(PROBE_POLL);
    }
}

/// Collect the launched child: `open` itself for the packaged helper, the
/// probe for a bare binary. A child still running at the deadline is killed.
fn reap(mut child: Child, deadline: Instant) {
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            Ok(None) => std::thread::sleep(PROBE_POLL),
        }
    }
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Schedule a relaunch to use a grant made after this process asked, unless
/// one already happened within [`RELAUNCH_COOLDOWN`]. Returns whether it was
/// scheduled; the lifecycle performs the graceful exit.
pub(crate) fn schedule_relaunch_for_late_grant() -> bool {
    let marker = match openlogi_core::paths::runtime_dir() {
        Ok(dir) => dir.join("input-monitoring-relaunch"),
        Err(error) => {
            warn!(%error, "no runtime dir for the Input Monitoring relaunch record — not relaunching");
            return false;
        }
    };
    let now = SystemTime::now();
    let last = std::fs::read_to_string(&marker)
        .ok()
        .and_then(|text| decode_time(&text));
    if !may_relaunch(last, now) {
        warn!(
            "Input Monitoring is granted, but the agent relaunched for it less than a minute ago and still cannot use it — log out of macOS and back in"
        );
        return false;
    }
    if let Err(error) = write_atomically(&marker, &encode_time(now)) {
        warn!(%error, path = %marker.display(), "could not record the Input Monitoring relaunch — not relaunching");
        return false;
    }
    crate::binary_watch::schedule_after_input_monitoring_grant()
}

/// Whether a relaunch may follow one recorded at `last`. A clock that moved
/// backwards past the record allows it rather than blocking forever.
fn may_relaunch(last: Option<SystemTime>, now: SystemTime) -> bool {
    last.and_then(|last| now.duration_since(last).ok())
        .is_none_or(|elapsed| elapsed >= RELAUNCH_COOLDOWN)
}

fn encode_time(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    format!("{secs}\n")
}

fn decode_time(text: &str) -> Option<SystemTime> {
    let secs = text.trim_end().parse().ok()?;
    UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> impl Iterator<Item = OsString> {
        list.iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn only_the_exact_probe_request_makes_a_probe() {
        assert_eq!(
            probe_result_path(args(&[PROBE_ARG, "/run/answer"])),
            Some(PathBuf::from("/run/answer"))
        );
        // A plain agent start, a missing path, a trailing argument, and some
        // other flag all run the agent instead.
        for list in [
            &[][..],
            &[PROBE_ARG][..],
            &[PROBE_ARG, "/run/answer", "extra"][..],
            &["--other", "/run/answer"][..],
        ] {
            assert_eq!(probe_result_path(args(list)), None, "args {list:?}");
        }
    }

    #[test]
    fn answers_round_trip_and_garbage_is_no_answer() {
        for answer in [Answer::Granted, Answer::Missing] {
            assert_eq!(Answer::decode(answer.encode()), Some(answer));
        }
        assert_eq!(Answer::decode(""), None);
        assert_eq!(Answer::decode("grant"), None);
        assert_eq!(Answer::decode("granted\nmissing\n"), None);
    }

    #[test]
    fn a_packaged_helper_probes_through_launchservices() {
        let exe = Path::new(
            "/Applications/OpenLogi.app/Contents/Helpers/OpenLogi Agent.app/Contents/MacOS/openlogi-agent",
        );
        let command = probe_command(exe, Path::new("/run/answer"));
        assert_eq!(command.get_program(), "/usr/bin/open");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            [
                "-g",
                "-n",
                "/Applications/OpenLogi.app/Contents/Helpers/OpenLogi Agent.app",
                "--args",
                PROBE_ARG,
                "/run/answer",
            ]
        );
    }

    #[test]
    fn a_bare_binary_probes_directly() {
        let exe = Path::new("/work/target/debug/openlogi-agent");
        let command = probe_command(exe, Path::new("/run/answer"));
        assert_eq!(command.get_program(), exe);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, [PROBE_ARG, "/run/answer"]);
    }

    #[test]
    fn a_relaunch_waits_out_the_cooldown() {
        let then = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let second = Duration::from_secs(1);
        assert!(may_relaunch(None, then), "no earlier relaunch");
        assert!(!may_relaunch(
            Some(then),
            then + RELAUNCH_COOLDOWN.saturating_sub(second)
        ));
        assert!(may_relaunch(Some(then), then + RELAUNCH_COOLDOWN));
        // A clock set back before the record must not block relaunches.
        let earlier = then.checked_sub(5 * second).unwrap();
        assert!(may_relaunch(Some(then), earlier));
    }

    #[test]
    fn the_relaunch_record_round_trips() {
        let time = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(decode_time(&encode_time(time)), Some(time));
        assert_eq!(decode_time("soon"), None);
    }

    #[test]
    fn a_probe_answer_is_read_from_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = dir.path().join("answer");
        write_atomically(&result, Answer::Granted.encode()).unwrap();
        assert!(
            !staged_path(&result).exists(),
            "the staged file is renamed away"
        );
        let answer = wait_for_answer(&result, Instant::now()).unwrap();
        assert_eq!(answer, Answer::Granted);
    }

    #[test]
    fn every_attempt_has_its_own_answer_and_staged_file() {
        let first = next_attempt();
        let second = next_attempt();
        assert!(second > first, "attempt numbers never repeat");
        let dir = Path::new("/run");
        let early = dir.join(answer_name(7, first));
        let late = dir.join(answer_name(7, second));
        assert_ne!(early, late);
        assert_ne!(staged_path(&early), staged_path(&late));
        assert_eq!(
            staged_path(Path::new("/run/input-monitoring-probe.7.3")),
            Path::new("/run/input-monitoring-probe.7.3.tmp")
        );
    }

    #[test]
    fn the_sweep_removes_late_answers_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let late = dir.path().join(answer_name(7, 0));
        write_atomically(&late, Answer::Granted.encode()).unwrap();
        std::fs::write(staged_path(&dir.path().join(answer_name(7, 1))), "gran").unwrap();
        let marker = dir.path().join("input-monitoring-relaunch");
        std::fs::write(&marker, encode_time(UNIX_EPOCH)).unwrap();

        sweep_answers(dir.path());

        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(left, [marker], "only the relaunch record survives");
    }

    #[test]
    fn a_probe_that_never_answers_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let error = wait_for_answer(&dir.path().join("answer"), Instant::now()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
