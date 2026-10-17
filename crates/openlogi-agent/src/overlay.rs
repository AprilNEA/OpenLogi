//! Supervision of the Actions Ring overlay helper.
//!
//! The helper owns no device state and exits harmlessly when its binary is not
//! packaged. A ring, notice or keyboard indicator starts it on demand; a late
//! helper reads the state the agent has already published. A transition to an
//! online device binding ShowActionsRing also pre-warms it. After ten idle
//! minutes it leaves with [`OVERLAY_IDLE_EXIT_CODE`], even if that binding is
//! still present. This is not a crash: the next ring or indicator cold-starts
//! a helper, unless a new pre-warm transition starts one first ([`Schedule`]).
//!
//! Exactly one overlay may exist, and it belongs to one agent run. Both halves
//! are enforced by the `succession` crate: this supervisor waits while the role
//! is filled by its own child, and evicts a tenant left behind by a previous
//! agent — which is what stops an orphaned overlay from wedging its
//! replacement out of the lock forever (#621, #644). The run token travels to
//! the child in the environment, so a helper started by a previous agent is
//! recognizable on sight rather than after a timeout.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant};

use openlogi_core::brand;
use openlogi_ipc::{OVERLAY_IDLE_EXIT_CODE, RUN_ENV};
use succession::eviction::{self, AnonymousOutcome, Policy};
use succession::supervision::{Event, Restart, Supervisor};
use succession::{Role, Run};
use tracing::{debug, info, warn};

/// The agent's handle on the overlay supervisor thread.
pub(crate) struct Overlay {
    /// `None` when no helper can run here — no binary, no config directory,
    /// or no thread — which makes every nudge a no-op.
    nudges: Option<Sender<Nudge>>,
}

/// What the agent tells the supervisor thread.
enum Nudge {
    /// A ring, notice or keyboard indicator needs drawing.
    RingOpened,
    /// Whether a device in hand can open the ring changed to this answer.
    Prewarm(bool),
}

impl Overlay {
    /// Start the supervisor thread, parked: no helper runs until a ring opens
    /// or [`Self::prewarm`] first says that a device in hand can open one.
    ///
    /// `ring_showing` runs on the supervisor thread and reports whether a
    /// ring, notice or indicator is open that a helper has to draw.
    pub(crate) fn start(ring_showing: impl Fn() -> bool + Send + 'static) -> Self {
        let Some(binary) = overlay_binary_path() else {
            warn!("Actions Ring overlay binary not found — overlay disabled");
            return Self { nudges: None };
        };
        let Ok(directory) = openlogi_core::paths::config_dir() else {
            warn!("could not resolve the config directory — overlay disabled");
            return Self { nudges: None };
        };
        let (nudges, inbox) = mpsc::channel();
        let result = std::thread::Builder::new()
            .name("openlogi-overlay-supervisor".into())
            .spawn(move || supervise(&binary, &directory, &inbox, ring_showing));
        if let Err(error) = result {
            warn!(%error, "could not start the Actions Ring overlay supervisor");
            return Self { nudges: None };
        }
        Self {
            nudges: Some(nudges),
        }
    }

    /// Content needs drawing. Call after publishing it, so a supervisor
    /// deciding about a helper that just exited sees the current demand.
    pub(crate) fn show_requested(&self) {
        self.nudge(Nudge::RingOpened);
    }

    /// The pre-warm answer changed: `wanted` says whether a device in hand
    /// can now open the ring.
    pub(crate) fn prewarm(&self, wanted: bool) {
        self.nudge(Nudge::Prewarm(wanted));
    }

    fn nudge(&self, nudge: Nudge) {
        if let Some(nudges) = &self.nudges {
            // Only a supervisor thread that has gone drops the receiver, and
            // then nothing is left to start a helper either way.
            let _ = nudges.send(nudge);
        }
    }
}

/// The supervisor thread: park until [`Schedule`] wants a helper, then keep
/// the role filled for as long as it does.
fn supervise(
    binary: &Path,
    directory: &Path,
    inbox: &Receiver<Nudge>,
    ring_showing: impl Fn() -> bool,
) {
    let mine = Run::mint();
    // The crash backoff is applied below rather than by `succession`, whose
    // sleep runs inside `tick` after every exit: there it would hold the next
    // ring behind an idle exit too, and no ring opening could cut it short.
    let backoff = Restart::default();
    let mut supervisor = Supervisor::new(Role::new(directory, "overlay"), mine).restart(Restart {
        base: Duration::ZERO,
        ceiling: Duration::ZERO,
        ..backoff
    });
    let mut delay = backoff.base;
    let mut spawn = move || {
        Command::new(binary)
            .env(RUN_ENV, mine.get().to_string())
            .spawn()
    };
    let mut schedule = Schedule::default();
    // The anonymous verdict repeats every poll for as long as the tenant
    // lives, and answering it walks the process table. Answer once per spell
    // of anonymity and stay quiet until the role changes hands.
    let mut pressed_anonymous = false;
    loop {
        if !schedule.active() {
            let Ok(nudge) = inbox.recv() else {
                return;
            };
            match nudge {
                Nudge::RingOpened => {
                    schedule.ring_opened();
                    info!("starting the Actions Ring overlay for the ring that just opened");
                }
                Nudge::Prewarm(wanted) => {
                    schedule.prewarm_answered(wanted);
                    if schedule.active() {
                        info!("starting the Actions Ring overlay ahead of its first ring");
                    }
                }
            }
            continue;
        }
        let mut ended = None;
        let mut ran_for = Duration::ZERO;
        if let Err(error) = supervisor.tick(&mut spawn, &mut |event| {
            if let Some(how) = Ended::from_event(&event) {
                ended = Some(how);
            }
            if let Event::Exited { ran_for: run, .. } = &event {
                ran_for = *run;
            }
            report(&event, &mut pressed_anonymous);
        }) {
            // A role that cannot be probed is treated as free by the next
            // tick; refusing to look again would wait forever.
            warn!(%error, "could not read the Actions Ring overlay role");
        }
        if ended.is_some() {
            delay = backoff.next_delay(delay, ran_for);
        }
        // Rings opened while that helper ran were drawn by it, or are still
        // open for the next one — `ring_showing` below tells which.
        loop {
            match inbox.try_recv() {
                Ok(Nudge::RingOpened) => {}
                Ok(Nudge::Prewarm(wanted)) => schedule.prewarm_answered(wanted),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        let ring = || {
            if ring_showing() {
                Ring::Open
            } else {
                Ring::Closed
            }
        };
        schedule.ended(ended, ring());
        // Only a helper that did not choose to leave is restarted after a
        // pause. An idle exit is not a crash, and an occupied role was already
        // waited on inside `tick`.
        if schedule.active() && ended == Some(Ended::Other) {
            info!("waiting {delay:.1?} before restarting the Actions Ring overlay");
            if !back_off(inbox, delay, &mut schedule) {
                return;
            }
            // Answers that arrived meanwhile may have withdrawn the restart.
            schedule.ended(ended, ring());
        }
        if !schedule.active() {
            match ended {
                Some(Ended::Idle) => {
                    info!("the Actions Ring overlay left after idling — the next ring starts it");
                }
                Some(Ended::Other) => info!(
                    "not restarting the Actions Ring overlay — no device in hand opens the \
                     ring, so the next ring starts it"
                ),
                None => debug!("Actions Ring overlay supervisor parked"),
            }
        }
    }
}

/// Sit out a crash backoff of `delay`, cut short by a ring opening: that ring
/// is waiting to be drawn, and one start per press cannot spin. Pre-warm
/// answers that arrive meanwhile are recorded. Returns `false` once the agent
/// is gone.
fn back_off(inbox: &Receiver<Nudge>, delay: Duration, schedule: &mut Schedule) -> bool {
    let deadline = Instant::now() + delay;
    loop {
        let rest = deadline.saturating_duration_since(Instant::now());
        if rest.is_zero() {
            return true;
        }
        match inbox.recv_timeout(rest) {
            Ok(Nudge::RingOpened) | Err(RecvTimeoutError::Timeout) => return true,
            Ok(Nudge::Prewarm(wanted)) => schedule.prewarm_answered(wanted),
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
}

/// How a helper run ended, as far as starting another is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ended {
    /// It left with [`OVERLAY_IDLE_EXIT_CODE`]: nothing to show, so not wanted.
    Idle,
    /// Anything else — a crash, a failed start, a kill, a stand-down — which
    /// is restarted while a helper is wanted, as every exit used to be.
    Other,
}

impl Ended {
    /// What a supervisor event says about the run, if it ended one.
    fn from_event(event: &Event<'_>) -> Option<Self> {
        match event {
            Event::Exited { status, .. } => Some(Self::of_code(status.code())),
            Event::SpawnFailed(_) => Some(Self::Other),
            _ => None,
        }
    }

    /// Classify an exit code; `None` is a Unix signal death.
    fn of_code(code: Option<i32>) -> Self {
        if code == Some(OVERLAY_IDLE_EXIT_CODE) {
            Self::Idle
        } else {
            Self::Other
        }
    }
}

/// Whether a ring session is open that a helper has to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ring {
    Open,
    Closed,
}

/// When a helper should be running: the decision, without the process.
///
/// Two things start a parked supervisor — a ring opening, and the pre-warm
/// answer turning true. Once started it keeps a helper up across failures for
/// as long as one is wanted; an idle exit is the helper saying it is not, and
/// that stands until one of the two starters fires again. Whatever the exit,
/// an open ring restarts it: that ring is waiting to be drawn, and the next
/// helper joins late and draws it.
#[derive(Debug, Default)]
struct Schedule {
    /// The last pre-warm answer: a device in hand can open the ring.
    prewarm: bool,
    /// Whether the supervisor should fill the role rather than park.
    active: bool,
}

impl Schedule {
    const fn active(&self) -> bool {
        self.active
    }

    const fn ring_opened(&mut self) {
        self.active = true;
    }

    const fn prewarm_answered(&mut self, wanted: bool) {
        // Only the edge starts a helper. An answer that stays true across an
        // idle exit must not undo that exit on the next rebinding.
        if wanted && !self.prewarm {
            self.active = true;
        }
        self.prewarm = wanted;
    }

    /// A supervisor round finished; `ended` is `None` when it started no
    /// helper because the role was occupied.
    const fn ended(&mut self, ended: Option<Ended>, ring: Ring) {
        self.active = match (ring, ended) {
            (Ring::Open, _) => true,
            (Ring::Closed, Some(Ended::Idle)) => false,
            (Ring::Closed, Some(Ended::Other) | None) => self.prewarm,
        };
    }
}

/// Ask the overlay to leave, on the way out of a deliberate agent shutdown.
///
/// The helper is spawned detached and the menu-bar Quit is a `process::exit`
/// that runs no destructors, so without this the overlay outlives the agent
/// until its own give-up deadline — a minute of a stray GPUI process in
/// Activity Monitor after the user asked for everything to stop. Nothing here
/// is load-bearing: the overlay leaves either way, so the policy is tuned for a
/// Quit that still feels instant rather than for a guaranteed exit.
///
/// Only the tray platforms have a deliberate shutdown to hook. Elsewhere the
/// agent runs until something kills it, and the overlay's own deadline is all
/// there is.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn evict_on_quit() {
    use std::time::Duration;

    use succession::Occupancy;

    let Ok(directory) = openlogi_core::paths::config_dir() else {
        return;
    };
    let Ok(Occupancy::HeldBy(record)) = Role::new(directory, "overlay").occupancy() else {
        return;
    };
    let outcome = eviction::evict(
        &record,
        &Policy {
            escalate_after: Some(Duration::from_millis(150)),
            deadline: Duration::from_millis(750),
            ..Policy::default()
        },
    );
    info!(?outcome, "asked the overlay to leave before exiting");
}

/// Log what the supervisor did, and evict a tenant this agent has superseded.
///
/// Eviction is the migration path: an overlay that predates the claim record
/// cannot recognize this agent as a different run, so it never yields on its
/// own. Which of the two evictions applies depends on what the tenant said
/// about itself, and neither takes a pid on faith — a record is checked
/// against the live process ([`succession::Tenant::compare`]), and a tenant
/// with no record at all is only ever recognized by the image we start the
/// overlay from.
fn report(event: &Event<'_>, pressed_anonymous: &mut bool) {
    if !matches!(event, Event::SupersededAnonymously) {
        *pressed_anonymous = false;
    }
    match *event {
        Event::Superseded(record) => {
            info!("{event}");
            match eviction::evict(record, &Policy::default()) {
                eviction::Outcome::Refused(sameness) => {
                    warn!(
                        ?sameness,
                        "left the overlay alone — its pid no longer matches"
                    );
                }
                outcome => info!(?outcome, "asked the superseded overlay to leave"),
            }
        }
        // A tenant holding the role with no readable claim record: an overlay
        // from an install that predates the record, or one whose publish
        // failed. Nothing identifies it, so `succession` falls back on the
        // image we start the overlay from and refuses unless exactly one
        // process matches — otherwise the role stays wedged for as long as
        // that process lives and the Actions Ring never comes up (#842).
        Event::SupersededAnonymously => {
            if std::mem::replace(pressed_anonymous, true) {
                tracing::debug!("{event}");
                return;
            }
            warn!("{event}");
            evict_unidentified_overlay();
        }
        Event::Occupied(_) => tracing::debug!("{event}"),
        // Always zero here: `supervise` applies and logs the real backoff.
        Event::BackingOff(_) => {}
        _ => info!("{event}"),
    }
}

/// Ask an unidentified tenant to leave, trying every image our overlay could
/// be running from.
///
/// One path is not enough. A tenant started before an update runs the image
/// that install shipped, and macOS keeps reporting that path for the life of
/// the process even after the file is renamed or deleted — so the tenant most
/// in need of evicting is precisely the one whose image is not the one we
/// would launch today (#842).
fn evict_unidentified_overlay() {
    for image in overlay_images() {
        match eviction::evict_anonymous(&image, &Policy::default()) {
            // Not this image. The tenant may be running another of ours.
            AnonymousOutcome::NoCandidate => {}
            AnonymousOutcome::Ambiguous { running } => {
                warn!(
                    running,
                    image = %image.display(),
                    "several processes share this overlay image — left them alone rather \
                     than guess which one holds the role"
                );
                return;
            }
            outcome => {
                info!(
                    ?outcome,
                    image = %image.display(),
                    "asked the unidentified overlay to leave"
                );
                return;
            }
        }
    }
    warn!("no process is running any overlay image of ours — the role is held by something else");
}

/// Every path our overlay could be running from, in the order the launcher
/// prefers them.
///
/// Existence is deliberately not a filter: a process outlives the file it was
/// started from, and evicting one means recognizing the path it still reports.
/// [`overlay_binary_path`] applies the filter, because launching does need a
/// file that is there.
fn overlay_images() -> Vec<PathBuf> {
    let Ok(executable) = std::env::current_exe() else {
        return Vec::new();
    };
    let mut images = overlay_images_beside(&executable);
    images.extend(find_on_path(brand::Helper::Overlay.executable()));
    images
}

/// The layout-derived half of [`overlay_images`], taking the agent's own path
/// so the derivation can be tested against a bundle that is not this one.
fn overlay_images_beside(executable: &Path) -> Vec<PathBuf> {
    let sibling = executable.parent().map(|directory| {
        directory.join(format!(
            "{}{}",
            brand::Helper::Overlay.executable(),
            std::env::consts::EXE_SUFFIX
        ))
    });
    sibling
        .into_iter()
        .chain(bundled_overlay_images(executable))
        .collect()
}

/// Every helper bundle an agent at `executable` could have shipped with, on
/// the platform that bundles its helpers.
#[cfg(target_os = "macos")]
fn bundled_overlay_images(executable: &Path) -> Vec<PathBuf> {
    executable
        .ancestors()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        })
        .flat_map(|app| {
            brand::Helper::Overlay
                .executable_candidates()
                .into_iter()
                .map(move |relative| app.join(relative))
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn bundled_overlay_images(_executable: &Path) -> Vec<PathBuf> {
    Vec::new()
}

fn overlay_binary_path() -> Option<PathBuf> {
    overlay_images().into_iter().find(|image| image.is_file())
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// The tenant that wedges the role is one started before an update, and
    /// after the helpers were renamed its image is a path that no longer
    /// exists. macOS keeps reporting that path for the life of the process, so
    /// dropping absent paths here would leave exactly that tenant
    /// unrecognizable (#842).
    #[test]
    #[cfg(target_os = "macos")]
    fn eviction_candidates_include_images_that_are_no_longer_installed() {
        let agent = Path::new(
            "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Agent.app/Contents/MacOS/openlogi-agent",
        );
        let legacy = Path::new(
            "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogiOverlay.app/Contents/MacOS/openlogi-overlay",
        );
        assert!(
            !legacy.exists(),
            "this test only means something while that path is absent"
        );

        let images = overlay_images_beside(agent);
        assert!(
            images.iter().any(|image| image == legacy),
            "the pre-rename image must stay a candidate: {images:?}"
        );
    }

    /// A schedule that is running a helper, with the given pre-warm answer.
    fn running(prewarm: bool) -> Schedule {
        let mut schedule = Schedule::default();
        schedule.prewarm_answered(prewarm);
        schedule.ring_opened();
        schedule
    }

    #[test]
    fn only_the_idle_status_reads_as_idle() {
        assert_eq!(Ended::of_code(Some(OVERLAY_IDLE_EXIT_CODE)), Ended::Idle);
        // A stand-down, `main` returning an error, a Rust panic, and a Unix
        // signal death are all exits the helper did not choose for idleness.
        for code in [Some(0), Some(1), Some(101), None] {
            assert_eq!(Ended::of_code(code), Ended::Other, "{code:?}");
        }
    }

    #[test]
    fn nothing_runs_until_a_ring_or_a_ring_device_appears() {
        let mut schedule = Schedule::default();
        assert!(!schedule.active());
        schedule.prewarm_answered(false);
        assert!(
            !schedule.active(),
            "a desk with no ring device keeps no overlay"
        );
        schedule.ring_opened();
        assert!(
            schedule.active(),
            "a ring is drawn whatever the pre-warm answer"
        );
    }

    #[test]
    fn a_device_that_can_open_the_ring_starts_the_overlay_ahead_of_time() {
        let mut schedule = Schedule::default();
        schedule.prewarm_answered(true);
        assert!(schedule.active());
    }

    #[test]
    fn an_idle_exit_stands_until_a_ring_or_a_new_ring_device() {
        let mut schedule = running(true);
        schedule.ended(Some(Ended::Idle), Ring::Closed);
        assert!(!schedule.active(), "an idle exit is not a crash");
        schedule.prewarm_answered(true);
        assert!(
            !schedule.active(),
            "rebinding with the same answer must not undo the idle exit"
        );
        schedule.prewarm_answered(false);
        schedule.prewarm_answered(true);
        assert!(
            schedule.active(),
            "a ring device that comes back is warmed again"
        );
        schedule.ended(Some(Ended::Idle), Ring::Closed);
        schedule.ring_opened();
        assert!(schedule.active());
    }

    #[test]
    fn a_wanted_overlay_survives_its_crashes_and_an_unwanted_one_does_not() {
        let mut wanted = running(true);
        wanted.ended(Some(Ended::Other), Ring::Closed);
        assert!(wanted.active());

        let mut unwanted = running(false);
        unwanted.ended(Some(Ended::Other), Ring::Closed);
        assert!(
            !unwanted.active(),
            "the next ring starts it, not the backoff"
        );
    }

    #[test]
    fn a_device_that_stops_opening_the_ring_lets_a_crashed_overlay_rest() {
        let mut schedule = running(true);
        schedule.prewarm_answered(false);
        assert!(schedule.active(), "a running helper is left to idle out");
        schedule.ended(Some(Ended::Other), Ring::Closed);
        assert!(!schedule.active());
    }

    /// The whole decision after a round, for both pre-warm answers.
    #[test]
    fn a_finished_round_restarts_exactly_when_a_helper_is_still_wanted() {
        let cases = [
            // An open ring is waiting to be drawn, however the helper left.
            (false, Ring::Open, Some(Ended::Idle), true),
            (false, Ring::Open, Some(Ended::Other), true),
            (false, Ring::Open, None, true),
            (true, Ring::Open, Some(Ended::Idle), true),
            (true, Ring::Open, Some(Ended::Other), true),
            (true, Ring::Open, None, true),
            // With none open, idleness is final and the rest follow pre-warm.
            (false, Ring::Closed, Some(Ended::Idle), false),
            (false, Ring::Closed, Some(Ended::Other), false),
            (false, Ring::Closed, None, false),
            (true, Ring::Closed, Some(Ended::Idle), false),
            (true, Ring::Closed, Some(Ended::Other), true),
            (true, Ring::Closed, None, true),
        ];
        for (prewarm, ring, ended, restarts) in cases {
            let mut schedule = running(prewarm);
            schedule.ended(ended, ring);
            assert_eq!(
                schedule.active(),
                restarts,
                "prewarm {prewarm}, ring {ring:?}, ended {ended:?}"
            );
        }
    }

    #[test]
    fn a_ring_cuts_a_crash_backoff_short() {
        let (nudges, inbox) = mpsc::channel();
        let mut schedule = running(false);
        nudges.send(Nudge::Prewarm(true)).unwrap();
        nudges.send(Nudge::RingOpened).unwrap();
        let started = Instant::now();
        assert!(back_off(&inbox, Duration::from_mins(1), &mut schedule));
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the ring waited out the backoff"
        );
        assert!(schedule.prewarm, "an answer that arrived meanwhile is kept");
    }

    #[test]
    fn a_backoff_ends_with_the_agent() {
        let (nudges, inbox) = mpsc::channel::<Nudge>();
        drop(nudges);
        assert!(!back_off(
            &inbox,
            Duration::from_mins(1),
            &mut Schedule::default()
        ));
    }

    #[test]
    fn path_search_returns_none_for_an_impossible_name() {
        assert_eq!(
            find_on_path("openlogi-overlay-this-file-does-not-exist"),
            None
        );
    }

    #[test]
    fn nested_overlay_path_has_expected_layout() {
        let outer = Path::new("/Applications/OpenLogi.app");
        assert_eq!(
            outer.join(
                "Contents/Library/LoginItems/OpenLogi Overlay.app/Contents/MacOS/openlogi-overlay"
            ),
            Path::new(
                "/Applications/OpenLogi.app/Contents/Library/LoginItems/OpenLogi Overlay.app/Contents/MacOS/openlogi-overlay"
            )
        );
    }
}
