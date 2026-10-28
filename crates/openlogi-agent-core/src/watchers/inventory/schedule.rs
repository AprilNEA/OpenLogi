//! Deterministic scheduling policy for event-first inventory reconciliation.

use std::time::{Duration, Instant, SystemTime};

use openlogi_hid::inventory::PROBE_CACHE_FRESHNESS;
use openlogi_hid::inventory::events::HidppEventSource;

/// A bounded repair/confirmation retry delay. This keeps the old two-second
/// cadence only while an explicit lifecycle contract still needs it.
pub(super) const FAST_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Pause after an OS node event while the HID interface set settles.
pub(super) const HOTPLUG_SETTLE: Duration = Duration::from_millis(400);

/// Pause after a HID++ burst so one reconciliation covers the whole burst.
pub(super) const HID_EVENT_SETTLE: Duration = Duration::from_millis(200);

/// Pause after resume before probing devices that may still be reconnecting.
pub(super) const SYSTEM_RESUME_SETTLE: Duration = Duration::from_millis(400);

/// Spare time inside [`PROBE_CACHE_FRESHNESS`] for a late timer wake.
///
/// The cache stays fresh through that window and is stale after it. A recovery
/// scan that waits the whole window, or wakes a moment later, re-walks the
/// feature table (#1774).
const RECOVERY_WAKE_MARGIN: Duration = Duration::from_secs(1);

/// Authoritative recovery cadence when no trustworthy event arrives.
///
/// Receiver notifications and OS hotplug can be disabled, unsupported, or
/// missed across firmware/host transitions. Unified battery has an event, but
/// legacy `0x1000` and voltage `0x1001` batteries do not. The period sits
/// [`RECOVERY_WAKE_MARGIN`] inside the probe-cache window so a slightly late
/// wake still reuses the feature table, keeps those readings on the same
/// cadence, and does not return to constant full HID scans.
pub(super) const RECOVERY_SCAN_PERIOD: Duration =
    PROBE_CACHE_FRESHNESS.saturating_sub(RECOVERY_WAKE_MARGIN);

/// Wall-clock lead over monotonic time that means the monotonic clock paused
/// during system sleep. NTP false positives only cause harmless re-apply.
const WAKE_GAP: Duration = Duration::from_mins(1);

/// Fast passes after the scan that first found an unhealthy node. Four are
/// enough to cross the ledger's second-failure retirement, let users release
/// the old channel, defer reopen for one pass, and then probe the replacement.
const FAST_RETRY_LIMIT: u8 = 4;

/// One explicit reason for an authoritative reconciliation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReconcileTrigger {
    /// First inventory after the agent arms.
    Initial,
    /// Bounded repair of a failed open/probe or pending miss grace.
    RepairRetry,
    /// A persistent HID++ channel reported lifecycle state.
    HidEvent(HidppEventSource),
    /// The host HID node set changed.
    Hotplug,
    /// Native notification or wall/monotonic gap reported system resume.
    SystemResume,
    /// Volatile setting writes still need their bounded confirmation pass.
    SettingsConfirmation,
    /// Low-frequency authority for unsupported or missed event paths.
    RecoveryScan,
}

/// A deadline owned by the inventory worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DeadlinePurpose {
    RepairRetry,
    SettingsConfirmation,
    RecoveryScan,
}

/// Scan-start and scan-end clocks for one reconciliation.
#[derive(Clone, Copy, Debug)]
pub(super) struct ScanClock {
    pub(super) started: Instant,
    pub(super) finished: Instant,
}

/// Enumeration failure cannot satisfy a request for a delivered snapshot.
#[derive(Clone, Copy)]
pub(super) enum ScanOutcome {
    Snapshot { needs_repair: bool },
    Failed,
}

enum Confirmation {
    Scheduled {
        due: Instant,
        failures: u8,
    },
    /// Fast enumeration attempts are exhausted, not the confirmation request.
    AwaitingRecovery,
}

/// Scheduling state only; all I/O stays in the watcher loop.
pub(super) struct Schedule {
    retry_due: Option<Instant>,
    confirmation: Option<Confirmation>,
    recovery_due: Instant,
    retry_count: u8,
}

impl Schedule {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            retry_due: None,
            confirmation: None,
            recovery_due: now + RECOVERY_SCAN_PERIOD,
            retry_count: 0,
        }
    }

    /// Next deadline and its purpose.
    ///
    /// A pending repair retry outranks recovery. The retry is anchored at scan
    /// completion, so a pass that outlasts the recovery period must not look
    /// due first and skip the pause.
    pub(super) fn next_deadline(&self) -> (Instant, DeadlinePurpose) {
        let mut next = (self.recovery_due, DeadlinePurpose::RecoveryScan);
        if let Some(at) = self.retry_due {
            next = (at, DeadlinePurpose::RepairRetry);
        }
        if let Some(Confirmation::Scheduled { due, .. }) = self.confirmation
            && due <= next.0
        {
            next = (due, DeadlinePurpose::SettingsConfirmation);
        }
        next
    }

    /// A successful low-frequency recovery also delivers a confirmation whose
    /// fast enumeration budget was exhausted. Other lifecycle snapshots do not
    /// consume the orchestrator's confirmation budget.
    pub(super) fn confirms_settings(&self, trigger: ReconcileTrigger) -> bool {
        trigger == ReconcileTrigger::SettingsConfirmation
            || (trigger == ReconcileTrigger::RecoveryScan
                && matches!(self.confirmation, Some(Confirmation::AwaitingRecovery)))
    }

    /// Retire a confirmation only when its snapshot reached the orchestrator.
    /// Failed enumeration gets bounded delayed attempts, then keeps the logical
    /// request for the ordinary recovery cadence. Repair scans cannot reset it.
    pub(super) fn scan_finished(
        &mut self,
        trigger: ReconcileTrigger,
        outcome: ScanOutcome,
        clock: ScanClock,
    ) {
        self.retry_due = None;
        match (self.confirms_settings(trigger), outcome) {
            (true, ScanOutcome::Snapshot { .. }) => self.confirmation = None,
            (true, ScanOutcome::Failed) => {
                if let Some(Confirmation::Scheduled { failures, .. }) = self.confirmation {
                    self.confirmation = Some(if failures + 1 < FAST_RETRY_LIMIT {
                        Confirmation::Scheduled {
                            due: clock.finished + FAST_RETRY_DELAY,
                            failures: failures + 1,
                        }
                    } else {
                        Confirmation::AwaitingRecovery
                    });
                }
            }
            (false, ScanOutcome::Snapshot { .. }) => {
                if matches!(self.confirmation, Some(Confirmation::AwaitingRecovery)) {
                    self.confirmation = None;
                    self.request_settings_confirmation(clock.finished);
                }
            }
            (false, ScanOutcome::Failed) => {}
        }
        self.recovery_due = clock.started + RECOVERY_SCAN_PERIOD;

        if !matches!(
            trigger,
            ReconcileTrigger::RepairRetry | ReconcileTrigger::SettingsConfirmation
        ) {
            self.retry_count = 0;
        }
        let needs_repair = match outcome {
            ScanOutcome::Snapshot { needs_repair } => needs_repair,
            ScanOutcome::Failed => true,
        };
        if needs_repair && self.retry_count < FAST_RETRY_LIMIT {
            self.retry_count += 1;
            self.retry_due = Some(clock.finished + FAST_RETRY_DELAY);
        } else if !needs_repair {
            self.retry_count = 0;
        }
    }

    /// Request one delayed settings-confirmation pass. Repeated requests
    /// coalesce without postponing the deadline or restarting an exhausted
    /// enumeration budget. Only a delivered confirmation satisfies the request.
    pub(super) fn request_settings_confirmation(&mut self, now: Instant) {
        self.confirmation.get_or_insert(Confirmation::Scheduled {
            due: now + FAST_RETRY_DELAY,
            failures: 0,
        });
    }
}

/// Detects suspend by comparing wall and monotonic elapsed time.
pub(super) struct WakeDetector {
    wall: SystemTime,
    monotonic: Instant,
}

impl WakeDetector {
    pub(super) fn new(wall: SystemTime, monotonic: Instant) -> Self {
        Self { wall, monotonic }
    }

    pub(super) fn observe(&mut self, wall: SystemTime, monotonic: Instant) -> bool {
        let monotonic_elapsed = monotonic.saturating_duration_since(self.monotonic);
        let woke = wall
            .duration_since(self.wall)
            .is_ok_and(|wall_elapsed| wall_elapsed > monotonic_elapsed + WAKE_GAP);
        self.wall = wall;
        self.monotonic = monotonic;
        woke
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed(at: Instant) -> ScanClock {
        ScanClock {
            started: at,
            finished: at,
        }
    }

    #[test]
    fn repair_retries_are_fast_but_bounded() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);

        schedule.scan_finished(
            ReconcileTrigger::Initial,
            ScanOutcome::Snapshot { needs_repair: true },
            closed(start),
        );
        for _ in 0..FAST_RETRY_LIMIT {
            let (due, purpose) = schedule.next_deadline();
            assert_eq!(purpose, DeadlinePurpose::RepairRetry);
            schedule.scan_finished(
                ReconcileTrigger::RepairRetry,
                ScanOutcome::Snapshot { needs_repair: true },
                closed(due),
            );
        }

        assert!(schedule.retry_due.is_none());
        assert_eq!(schedule.retry_count, FAST_RETRY_LIMIT);
    }

    #[test]
    fn healthy_pass_resets_the_repair_budget() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        schedule.scan_finished(
            ReconcileTrigger::Initial,
            ScanOutcome::Snapshot { needs_repair: true },
            closed(start),
        );
        let retry = schedule.next_deadline().0;
        schedule.scan_finished(
            ReconcileTrigger::RepairRetry,
            ScanOutcome::Snapshot {
                needs_repair: false,
            },
            closed(retry),
        );

        assert!(schedule.retry_due.is_none());
        assert_eq!(schedule.retry_count, 0);
    }

    #[test]
    fn settings_confirmation_has_one_explicit_delayed_deadline() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        schedule.request_settings_confirmation(start);
        schedule.request_settings_confirmation(start + Duration::from_secs(1));

        assert_eq!(
            schedule.next_deadline(),
            (
                start + FAST_RETRY_DELAY,
                DeadlinePurpose::SettingsConfirmation,
            )
        );
        schedule.scan_finished(
            ReconcileTrigger::SettingsConfirmation,
            ScanOutcome::Snapshot {
                needs_repair: false,
            },
            closed(start + FAST_RETRY_DELAY),
        );
        assert!(schedule.confirmation.is_none());
    }

    #[test]
    fn early_lifecycle_scans_do_not_consume_or_postpone_settings_confirmation() {
        let start = Instant::now();
        let due = start + FAST_RETRY_DELAY;
        let mut schedule = Schedule::new(start);
        schedule.request_settings_confirmation(start);

        schedule.scan_finished(
            ReconcileTrigger::HidEvent(HidppEventSource::UnifiedBattery),
            ScanOutcome::Snapshot {
                needs_repair: false,
            },
            closed(start + Duration::from_millis(500)),
        );
        schedule.request_settings_confirmation(start + Duration::from_millis(500));
        schedule.scan_finished(
            ReconcileTrigger::Hotplug,
            ScanOutcome::Snapshot {
                needs_repair: false,
            },
            closed(start + Duration::from_secs(1)),
        );
        schedule.request_settings_confirmation(start + Duration::from_secs(1));

        assert_eq!(
            schedule.next_deadline(),
            (due, DeadlinePurpose::SettingsConfirmation)
        );
    }

    #[test]
    fn failed_confirmation_is_delayed_and_failed_repairs_do_not_postpone_it() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        schedule.request_settings_confirmation(start);
        let failed_at = start + FAST_RETRY_DELAY;
        schedule.scan_finished(
            ReconcileTrigger::SettingsConfirmation,
            ScanOutcome::Failed,
            closed(failed_at),
        );
        let next = failed_at + FAST_RETRY_DELAY;
        schedule.scan_finished(
            ReconcileTrigger::RepairRetry,
            ScanOutcome::Failed,
            closed(failed_at + Duration::from_millis(500)),
        );
        schedule.request_settings_confirmation(failed_at + Duration::from_secs(1));
        assert_eq!(
            schedule.next_deadline(),
            (next, DeadlinePurpose::SettingsConfirmation)
        );
    }

    #[test]
    fn failed_confirmation_ties_exhaust_both_fast_budgets_without_losing_the_request() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        schedule.request_settings_confirmation(start);
        schedule.scan_finished(
            ReconcileTrigger::Initial,
            ScanOutcome::Failed,
            closed(start),
        );

        for attempt in 0..FAST_RETRY_LIMIT {
            let (due, purpose) = schedule.next_deadline();
            assert_eq!(
                purpose,
                DeadlinePurpose::SettingsConfirmation,
                "a tied repair must not starve confirmation"
            );
            assert_eq!(due, start + FAST_RETRY_DELAY * u32::from(attempt + 1));
            schedule.scan_finished(
                ReconcileTrigger::SettingsConfirmation,
                ScanOutcome::Failed,
                closed(due),
            );
            schedule.request_settings_confirmation(due);
        }

        assert_eq!(schedule.next_deadline().1, DeadlinePurpose::RecoveryScan);
        assert_eq!(schedule.retry_count, FAST_RETRY_LIMIT);
        assert!(schedule.confirms_settings(ReconcileTrigger::RecoveryScan));
        let recovery = schedule.next_deadline().0;
        schedule.scan_finished(
            ReconcileTrigger::RecoveryScan,
            ScanOutcome::Snapshot {
                needs_repair: false,
            },
            closed(recovery),
        );
        assert!(!schedule.confirms_settings(ReconcileTrigger::RecoveryScan));
        assert!(schedule.confirmation.is_none());
    }

    #[test]
    fn failed_low_frequency_recovery_keeps_confirmation_pending_through_repair() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        schedule.request_settings_confirmation(start);
        for _ in 0..FAST_RETRY_LIMIT {
            let (due, _) = schedule.next_deadline();
            schedule.scan_finished(
                ReconcileTrigger::SettingsConfirmation,
                ScanOutcome::Failed,
                closed(due),
            );
        }
        // A repair may remain after the initial confirmation failure armed it.
        for _ in 0..FAST_RETRY_LIMIT {
            if schedule.next_deadline().1 != DeadlinePurpose::RepairRetry {
                break;
            }
            let due = schedule.next_deadline().0;
            schedule.scan_finished(
                ReconcileTrigger::RepairRetry,
                ScanOutcome::Failed,
                closed(due),
            );
        }
        assert_eq!(schedule.next_deadline().1, DeadlinePurpose::RecoveryScan);
        let recovery = schedule.next_deadline().0;
        schedule.scan_finished(
            ReconcileTrigger::RecoveryScan,
            ScanOutcome::Failed,
            closed(recovery),
        );
        assert!(schedule.confirms_settings(ReconcileTrigger::RecoveryScan));
        let repaired = schedule.next_deadline().0;
        assert!(!schedule.confirms_settings(ReconcileTrigger::RepairRetry));
        schedule.scan_finished(
            ReconcileTrigger::RepairRetry,
            ScanOutcome::Snapshot {
                needs_repair: false,
            },
            closed(repaired),
        );
        assert_eq!(
            schedule.next_deadline(),
            (
                repaired + FAST_RETRY_DELAY,
                DeadlinePurpose::SettingsConfirmation
            )
        );
    }

    #[test]
    fn event_scan_reanchors_the_inactivity_recovery_deadline() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        let event_time = start + Duration::from_secs(30);
        schedule.scan_finished(
            ReconcileTrigger::HidEvent(HidppEventSource::UnifiedBattery),
            ScanOutcome::Snapshot {
                needs_repair: false,
            },
            closed(event_time),
        );

        assert_eq!(schedule.recovery_due, event_time + RECOVERY_SCAN_PERIOD);
    }

    #[test]
    fn late_recovery_wake_is_still_inside_the_probe_cache_window() {
        let started = Instant::now();
        let finished = started + Duration::from_secs(4);
        let mut schedule = Schedule::new(started);
        schedule.scan_finished(
            ReconcileTrigger::Initial,
            ScanOutcome::Snapshot {
                needs_repair: false,
            },
            ScanClock { started, finished },
        );

        let (due, purpose) = schedule.next_deadline();
        assert_eq!(purpose, DeadlinePurpose::RecoveryScan);
        assert_eq!(due, started + RECOVERY_SCAN_PERIOD);
        assert_eq!(
            RECOVERY_SCAN_PERIOD + RECOVERY_WAKE_MARGIN,
            PROBE_CACHE_FRESHNESS
        );
        let on_time = due;
        let late_by_margin = due + RECOVERY_WAKE_MARGIN;
        assert_eq!(late_by_margin, started + PROBE_CACHE_FRESHNESS);
        assert!(
            !openlogi_hid::inventory::probe_cache_is_stale(started, on_time),
            "an on-time recovery wake must hit the probe cache"
        );
        assert!(
            !openlogi_hid::inventory::probe_cache_is_stale(started, late_by_margin),
            "a wake late by the whole margin must still hit the probe cache"
        );
        assert!(
            openlogi_hid::inventory::probe_cache_is_stale(
                started,
                late_by_margin + Duration::from_nanos(1),
            ),
            "the cache is stale only after the freshness window"
        );
    }

    #[test]
    fn repair_pause_survives_a_scan_longer_than_the_retry_delay() {
        let started = Instant::now();
        let finished = started + Duration::from_secs(3);
        let mut schedule = Schedule::new(started);
        schedule.scan_finished(
            ReconcileTrigger::Initial,
            ScanOutcome::Snapshot { needs_repair: true },
            ScanClock { started, finished },
        );

        assert_eq!(
            schedule.next_deadline(),
            (finished + FAST_RETRY_DELAY, DeadlinePurpose::RepairRetry)
        );
        assert_eq!(schedule.recovery_due, started + RECOVERY_SCAN_PERIOD);
    }

    #[test]
    fn repair_pause_outranks_an_overdue_recovery_scan() {
        let started = Instant::now();
        let finished = started + RECOVERY_SCAN_PERIOD + FAST_RETRY_DELAY;
        let mut schedule = Schedule::new(started);
        schedule.scan_finished(
            ReconcileTrigger::Initial,
            ScanOutcome::Snapshot { needs_repair: true },
            ScanClock { started, finished },
        );

        assert!(
            schedule.recovery_due <= finished,
            "a scan longer than the recovery period leaves that deadline overdue"
        );
        assert_eq!(
            schedule.next_deadline(),
            (finished + FAST_RETRY_DELAY, DeadlinePurpose::RepairRetry)
        );
    }

    #[test]
    fn wake_detection_uses_wall_lead_not_a_slow_iteration() {
        let monotonic = Instant::now();
        let wall = SystemTime::now();
        let mut detector = WakeDetector::new(wall, monotonic);

        assert!(!detector.observe(
            wall + Duration::from_secs(70),
            monotonic + Duration::from_secs(70),
        ));
        assert!(detector.observe(
            wall + Duration::from_secs(140),
            monotonic + Duration::from_secs(72),
        ));
    }
}
