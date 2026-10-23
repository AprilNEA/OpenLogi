//! Agent-owned, bounded firmware activity history shared by real and mock jobs.

use std::time::{SystemTime, UNIX_EPOCH};

use openlogi_core::hid::{FirmwareUpdate, FirmwareUpdateEvent, FirmwareUpdatePhase};

const MAX_ACTIVITY_ENTRIES: usize = 128;

/// Publishes a phase change with an agent timestamp, retaining this job's history.
/// Transfer history records crossed 10% milestones while the current phase keeps
/// its full precision. Repeated phases do not notify observers. A new job, route,
/// or target starts a fresh history; caller-supplied history is not authoritative.
pub fn record_progress(current: &mut Option<FirmwareUpdate>, next: FirmwareUpdate) -> bool {
    record_at(current, next, || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok())
            .unwrap_or_default()
    })
}

fn record_at(
    current: &mut Option<FirmwareUpdate>,
    mut next: FirmwareUpdate,
    now: impl FnOnce() -> u64,
) -> bool {
    let previous = current.as_ref().filter(|previous| {
        previous.id == next.id && previous.route == next.route && previous.target == next.target
    });
    if previous
        .is_some_and(|previous| previous.phase == next.phase && previous.source == next.source)
    {
        return false;
    }
    next.events = previous.map_or_else(Vec::new, |previous| previous.events.clone());
    let phases = activity_phases(&next);
    if !phases.is_empty() {
        let timestamp_millis = now();
        next.events
            .extend(phases.into_iter().map(|phase| FirmwareUpdateEvent {
                timestamp_millis,
                phase,
            }));
        if next.events.len() > MAX_ACTIVITY_ENTRIES {
            next.events
                .drain(..next.events.len() - MAX_ACTIVITY_ENTRIES);
        }
    }
    *current = Some(next);
    true
}

fn activity_phases(update: &FirmwareUpdate) -> Vec<FirmwareUpdatePhase> {
    let FirmwareUpdatePhase::Transferring { percent } = update.phase else {
        return vec![update.phase.clone()];
    };
    let start = update
        .events
        .iter()
        .rev()
        .find_map(|event| match event.phase {
            FirmwareUpdatePhase::Transferring { percent } => Some(percent.saturating_add(10)),
            _ => None,
        })
        .unwrap_or(0);
    (start..=percent.min(100))
        .step_by(10)
        .map(|percent| FirmwareUpdatePhase::Transferring { percent })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::hid::{
        DeviceRoute, FirmwareUpdateFailure, FirmwareUpdatePhase, FirmwareVersion,
    };

    fn update(id: u64, phase: FirmwareUpdatePhase) -> FirmwareUpdate {
        FirmwareUpdate {
            source: None,
            route: DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb042,
            },
            target: FirmwareVersion {
                number: 27,
                revision: 3,
                build: 19,
            },
            phase,
            id,
            events: Vec::new(),
        }
    }

    #[test]
    fn firmware_activity_keeps_phase_times_deduplicates_and_retains_errors() {
        let mut current = None;
        assert!(record_at(
            &mut current,
            update(1, FirmwareUpdatePhase::Downloading),
            || 1000
        ));
        assert!(!record_at(
            &mut current,
            update(1, FirmwareUpdatePhase::Downloading),
            || panic!("duplicate phase must not read the clock")
        ));
        assert!(record_at(
            &mut current,
            update(1, FirmwareUpdatePhase::Preparing),
            || 2000
        ));
        let failure = FirmwareUpdatePhase::Failed(FirmwareUpdateFailure::ImageRejected);
        assert!(record_at(&mut current, update(1, failure.clone()), || 3000));
        let current = current.unwrap();
        assert_eq!(
            current
                .events
                .iter()
                .map(|entry| entry.timestamp_millis)
                .collect::<Vec<_>>(),
            [1000, 2000, 3000]
        );
        assert_eq!(current.events.last().unwrap().phase, failure);
    }

    #[test]
    fn firmware_activity_is_bounded_and_a_new_job_starts_fresh() {
        let mut current = None;
        for index in 0..200 {
            let phase = if index % 2 == 0 {
                FirmwareUpdatePhase::Downloading
            } else {
                FirmwareUpdatePhase::Preparing
            };
            record_at(&mut current, update(1, phase), || index);
        }
        let entries = &current.as_ref().unwrap().events;
        assert_eq!(entries.len(), MAX_ACTIVITY_ENTRIES);
        assert_eq!(entries.first().unwrap().timestamp_millis, 72);
        assert_eq!(entries.last().unwrap().timestamp_millis, 199);
        record_at(
            &mut current,
            update(2, FirmwareUpdatePhase::Downloading),
            || 500,
        );
        assert_eq!(current.unwrap().events.len(), 1);
    }

    #[test]
    fn firmware_activity_logs_ten_percent_steps_without_reducing_live_precision() {
        let mut current = None;
        record_at(
            &mut current,
            update(1, FirmwareUpdatePhase::Transferring { percent: 37 }),
            || 1000,
        );
        assert_eq!(
            current
                .as_ref()
                .unwrap()
                .events
                .iter()
                .map(|event| event.phase.clone())
                .collect::<Vec<_>>(),
            [0, 10, 20, 30].map(|percent| FirmwareUpdatePhase::Transferring { percent })
        );
        record_at(
            &mut current,
            update(1, FirmwareUpdatePhase::Transferring { percent: 39 }),
            || panic!("no new milestone"),
        );
        assert_eq!(
            current.as_ref().unwrap().phase,
            FirmwareUpdatePhase::Transferring { percent: 39 }
        );
        record_at(
            &mut current,
            update(1, FirmwareUpdatePhase::Transferring { percent: 100 }),
            || 2000,
        );
        assert_eq!(
            current
                .unwrap()
                .events
                .iter()
                .map(|event| event.phase.clone())
                .collect::<Vec<_>>(),
            (0..=100)
                .step_by(10)
                .map(|percent| FirmwareUpdatePhase::Transferring { percent })
                .collect::<Vec<_>>()
        );
    }
}
