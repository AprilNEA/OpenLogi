//! Deterministic scheduling policy for event-first inventory reconciliation.

use std::time::{Duration, Instant, SystemTime};

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

/// Authoritative recovery cadence when no trustworthy event arrives.
///
/// Receiver notifications and OS hotplug can be disabled, unsupported, or
/// missed across firmware/host transitions. Unified battery has an event, but
/// legacy `0x1000` and voltage `0x1001` batteries do not. Thirty seconds
/// preserves the probe cache's prior freshness bound, keeps those readings
/// useful, and bounds recovery without returning to constant full HID scans.
pub(super) const RECOVERY_SCAN_INTERVAL: Duration = Duration::from_secs(30);

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

impl ReconcileTrigger {
    /// Whether this trigger means device RAM may have reset even when the
    /// authoritative snapshot keeps the same route and online state.
    pub(super) fn reapplies_volatile_settings(self) -> bool {
        matches!(
            self,
            Self::Hotplug
                | Self::SystemResume
                | Self::HidEvent(
                    HidppEventSource::ReceiverConnection | HidppEventSource::WirelessDeviceStatus
                )
        )
    }
}

/// A deadline owned by the inventory worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DeadlinePurpose {
    RepairRetry,
    SettingsConfirmation,
    RecoveryScan,
}

/// Scheduling state only; all I/O stays in the watcher loop.
pub(super) struct Schedule {
    retry_due: Option<Instant>,
    settings_due: Option<Instant>,
    recovery_due: Instant,
    retry_count: u8,
}

impl Schedule {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            retry_due: None,
            settings_due: None,
            recovery_due: now + RECOVERY_SCAN_INTERVAL,
            retry_count: 0,
        }
    }

    /// Earliest deadline and its single purpose.
    pub(super) fn next_deadline(&self) -> (Instant, DeadlinePurpose) {
        let mut next = (self.recovery_due, DeadlinePurpose::RecoveryScan);
        for candidate in [
            self.retry_due.map(|at| (at, DeadlinePurpose::RepairRetry)),
            self.settings_due
                .map(|at| (at, DeadlinePurpose::SettingsConfirmation)),
        ]
        .into_iter()
        .flatten()
        {
            if candidate.0 < next.0 {
                next = candidate;
            }
        }
        next
    }

    /// Any completed full pass satisfies pending repair and settings deadlines.
    /// An unhealthy result then starts (or advances) one bounded repair run.
    pub(super) fn scan_finished(
        &mut self,
        trigger: ReconcileTrigger,
        needs_repair: bool,
        now: Instant,
    ) {
        self.retry_due = None;
        self.settings_due = None;
        self.recovery_due = now + RECOVERY_SCAN_INTERVAL;

        if !matches!(trigger, ReconcileTrigger::RepairRetry) {
            self.retry_count = 0;
        }
        if needs_repair && self.retry_count < FAST_RETRY_LIMIT {
            self.retry_count += 1;
            self.retry_due = Some(now + FAST_RETRY_DELAY);
        } else if !needs_repair {
            self.retry_count = 0;
        }
    }

    /// Request one delayed settings-confirmation pass. Repeated requests
    /// coalesce, and any intervening full scan satisfies the request.
    pub(super) fn request_settings_confirmation(&mut self, now: Instant) {
        self.settings_due.get_or_insert(now + FAST_RETRY_DELAY);
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

    #[test]
    fn repair_retries_are_fast_but_bounded() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);

        schedule.scan_finished(ReconcileTrigger::Initial, true, start);
        for _ in 0..FAST_RETRY_LIMIT {
            let (due, purpose) = schedule.next_deadline();
            assert_eq!(purpose, DeadlinePurpose::RepairRetry);
            schedule.scan_finished(ReconcileTrigger::RepairRetry, true, due);
        }

        assert!(schedule.retry_due.is_none());
        assert_eq!(schedule.retry_count, FAST_RETRY_LIMIT);
    }

    #[test]
    fn healthy_pass_resets_the_repair_budget() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        schedule.scan_finished(ReconcileTrigger::Initial, true, start);
        let retry = schedule.next_deadline().0;
        schedule.scan_finished(ReconcileTrigger::RepairRetry, false, retry);

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
            false,
            start + FAST_RETRY_DELAY,
        );
        assert!(schedule.settings_due.is_none());
    }

    #[test]
    fn event_scan_reanchors_the_inactivity_recovery_deadline() {
        let start = Instant::now();
        let mut schedule = Schedule::new(start);
        let event_time = start + Duration::from_secs(30);
        schedule.scan_finished(
            ReconcileTrigger::HidEvent(HidppEventSource::UnifiedBattery),
            false,
            event_time,
        );

        assert_eq!(schedule.recovery_due, event_time + RECOVERY_SCAN_INTERVAL);
    }

    #[test]
    fn reconnect_triggers_reapply_but_battery_events_do_not() {
        assert!(ReconcileTrigger::Hotplug.reapplies_volatile_settings());
        assert!(
            ReconcileTrigger::HidEvent(HidppEventSource::ReceiverConnection)
                .reapplies_volatile_settings()
        );
        assert!(
            ReconcileTrigger::HidEvent(HidppEventSource::WirelessDeviceStatus)
                .reapplies_volatile_settings()
        );
        assert!(
            !ReconcileTrigger::HidEvent(HidppEventSource::UnifiedBattery)
                .reapplies_volatile_settings()
        );
        assert!(!ReconcileTrigger::RecoveryScan.reapplies_volatile_settings());
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
