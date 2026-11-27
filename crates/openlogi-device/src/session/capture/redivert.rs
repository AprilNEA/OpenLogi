//! What a capture session does when another client changes one of its
//! `0x1b04` controls.
//!
//! Another client (Logi Options+, for example, or another OpenLogi channel to
//! the same HID node) can write `setCidReporting` for a control that this
//! session diverted. The device answers that client, and the response reaches
//! every open handle of the node; the `0x1b04` event stream publishes it
//! through [`reporting_changes`]. Without an answer, the session would keep a
//! diversion that the firmware no longer has, and the control would run its
//! native function until the next reconnect.
//!
//! [`reporting_changes`]: crate::reprog_controls::ReprogControlsV4::reporting_changes

use std::collections::BTreeMap;
use std::time::Duration;

use futures_lite::StreamExt as _;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use super::super::capture_restore::ArmedReporting;
use super::{ArmedCapture, ReportingChangeStream};
use crate::reprog_controls::{CidReportingChangeEcho, ReprogControlsV4};

/// The span over which the re-diverts of one control are counted.
const WINDOW: Duration = Duration::from_mins(1);

/// Re-diverts of one control allowed per [`WINDOW`].
const REDIVERTS_PER_WINDOW: u8 = 3;

/// What a [`RedivertBudget`] allows for one re-divert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Redivert {
    /// Divert the control again.
    Allowed,
    /// Leave the control: it used its budget for this window just now.
    Exhausted,
    /// Leave the control: it was already refused earlier in this window.
    StillExhausted,
}

/// The re-divert count of each control in one capture session.
///
/// Two clients that each divert a control again after the other hands it back
/// would trade `setCidReporting` writes as fast as the link carries them: the
/// response to each write is the other client's trigger. The budget ends such
/// a loop after a few rounds, and allows re-diverts again once the window
/// passes.
#[derive(Debug, Default)]
pub(super) struct RedivertBudget {
    windows: BTreeMap<u16, Window>,
}

/// One control's count in its current window.
#[derive(Debug)]
struct Window {
    opened: Instant,
    used: u8,
    refused: bool,
}

impl Window {
    fn open(now: Instant) -> Self {
        Self {
            opened: now,
            used: 0,
            refused: false,
        }
    }
}

impl RedivertBudget {
    /// Spend one re-divert of `cid` at `now`, when its window has one left.
    pub(super) fn take(&mut self, cid: u16, now: Instant) -> Redivert {
        let window = self.windows.entry(cid).or_insert_with(|| Window::open(now));
        if now.duration_since(window.opened) >= WINDOW {
            *window = Window::open(now);
        }
        if window.used < REDIVERTS_PER_WINDOW {
            window.used += 1;
            Redivert::Allowed
        } else if window.refused {
            Redivert::StillExhausted
        } else {
            window.refused = true;
            Redivert::Exhausted
        }
    }
}

/// How often a session reads its diverted controls back, to catch firmware
/// that drops a diversion without any event.
const AUDIT_INTERVAL: Duration = Duration::from_mins(1);

/// One capture session's watch over its diverted controls: the reporting
/// changes that other clients make, a periodic read-back for losses that no
/// event announces, and the re-divert budget that answers both.
pub(super) struct ReportingChanges {
    /// `None` when the session armed no `0x1b04` control, or once the
    /// subscription has closed.
    changes: Option<ReportingChangeStream>,
    audit: tokio::time::Interval,
    budget: RedivertBudget,
}

/// Why a session reads its diverted controls back.
#[derive(Debug, Clone, Copy)]
pub(super) enum Cause {
    /// Another client wrote `setCidReporting` for one control.
    Echo(CidReportingChangeEcho),
    /// The periodic audit of every control, with no event behind it.
    ///
    /// Some firmware drops a diversion with no `0x1b04` echo and no `0x1d4b`
    /// reconnect broadcast: the MX Keys S does it to its Lock key now and
    /// then, and the key then locks the screen.
    Audit,
}

impl ReportingChanges {
    pub(super) fn new(changes: Option<ReportingChangeStream>) -> Self {
        // The first audit is one interval out: arming just wrote every control.
        let mut audit = tokio::time::interval_at(Instant::now() + AUDIT_INTERVAL, AUDIT_INTERVAL);
        audit.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Self {
            changes,
            audit,
            budget: RedivertBudget::default(),
        }
    }

    /// The next reason to read controls back: a change that another client
    /// made, or a due audit.
    pub(super) async fn next(&mut self) -> Cause {
        loop {
            let Some(changes) = self.changes.as_mut() else {
                self.audit.tick().await;
                return Cause::Audit;
            };
            tokio::select! {
                echo = changes.next() => match echo {
                    Some(echo) => return Cause::Echo(echo),
                    None => self.changes = None,
                },
                _ = self.audit.tick() => return Cause::Audit,
            }
        }
    }

    /// Answer `cause` for the controls that `armed` owns.
    pub(super) async fn answer<A: ArmedCapture>(&mut self, armed: &A, cause: Cause) {
        match cause {
            Cause::Echo(echo) => redivert_after(armed, echo, &mut self.budget).await,
            Cause::Audit => {
                let Some((controls, reporting)) = armed.armed_reporting() else {
                    return;
                };
                for owned in reporting {
                    redivert_if_lost(controls, owned, cause, &mut self.budget, A::NAME).await;
                }
            }
        }
    }
}

/// Answer `echo`, another client's change to one control, for the controls
/// that `armed` owns.
async fn redivert_after<A: ArmedCapture>(
    armed: &A,
    echo: CidReportingChangeEcho,
    budget: &mut RedivertBudget,
) {
    let Some((controls, reporting)) = armed.armed_reporting() else {
        return;
    };
    let cid = echo.cid.0;
    let Some(owned) = reporting.iter().find(|owned| owned.cid == cid) else {
        debug!(
            cid = format_args!("{cid:#06x}"),
            capture = A::NAME,
            ?echo,
            "another client changed a control that capture does not divert"
        );
        return;
    };
    redivert_if_lost(controls, owned, Cause::Echo(echo), budget, A::NAME).await;
}

/// Read `owned` back, and divert it again when its diversion is gone and
/// `budget` allows it. Failures are logged, not propagated: the next audit or
/// reconnect tries again.
async fn redivert_if_lost(
    controls: &ReprogControlsV4,
    owned: &ArmedReporting,
    cause: Cause,
    budget: &mut RedivertBudget,
    capture: &'static str,
) {
    let cid = owned.cid;
    // An echo holds only the settings that the other client wrote, so read
    // the whole reporting state back.
    let current = match controls.get_cid_reporting(cid).await {
        Ok(current) => current,
        // An audit of a sleeping device fails every interval; only a failure
        // to answer an echo is worth a warning.
        Err(error) => {
            match cause {
                Cause::Echo(_) => {
                    warn!(cid = format_args!("{cid:#06x}"), capture, ?cause, %error, "cannot read a captured control back — leaving it until the next check");
                }
                Cause::Audit => {
                    debug!(cid = format_args!("{cid:#06x}"), capture, %error, "cannot read a captured control back — checking again next audit");
                }
            }
            return;
        }
    };
    let Some(change) = owned.redivert_change(current) else {
        if let Cause::Echo(echo) = cause {
            debug!(
                cid = format_args!("{cid:#06x}"),
                capture,
                ?echo,
                "another client changed a captured control; its diversion still holds"
            );
        }
        return;
    };
    match budget.take(cid, Instant::now()) {
        Redivert::Allowed => {}
        Redivert::Exhausted => {
            warn!(
                cid = format_args!("{cid:#06x}"),
                capture,
                ?cause,
                "a captured control keeps losing its diversion — leaving it native for now"
            );
            return;
        }
        Redivert::StillExhausted => return,
    }
    info!(
        cid = format_args!("{cid:#06x}"),
        capture,
        ?cause,
        "a captured control lost its diversion — diverting it again"
    );
    if let Err(error) = controls.set_cid_reporting_full(cid, change).await {
        warn!(cid = format_args!("{cid:#06x}"), capture, %error, "re-divert failed — control stays native until the next check");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reprog_controls::{CidReporting, CidReportingChange, ControlId};
    use crate::session::capture_restore::{ArmedReporting, PointerDivert};

    const CID: u16 = 0x00c3;

    fn reporting(diverted: bool, raw_xy: bool, remap: Option<ControlId>) -> CidReporting {
        CidReporting {
            cid: ControlId(CID),
            diverted,
            persistently_diverted: false,
            force_raw_xy: false,
            raw_xy,
            remap,
            analytics_key_events: false,
            raw_wheel: false,
        }
    }

    #[test]
    fn budget_refuses_after_the_window_is_spent_and_warns_once() {
        let mut budget = RedivertBudget::default();
        let start = Instant::now();
        for _ in 0..REDIVERTS_PER_WINDOW {
            assert_eq!(budget.take(CID, start), Redivert::Allowed);
        }
        assert_eq!(budget.take(CID, start), Redivert::Exhausted);
        assert_eq!(
            budget.take(CID, start + WINDOW / 2),
            Redivert::StillExhausted
        );
        // Another control has its own count.
        assert_eq!(budget.take(CID + 1, start), Redivert::Allowed);
        // A new window allows re-diverts again.
        assert_eq!(budget.take(CID, start + WINDOW), Redivert::Allowed);
    }

    #[test]
    fn a_change_that_undid_the_diversion_is_reverted_without_touching_the_remap() {
        let armed =
            ArmedReporting::pointer(CID, reporting(false, false, None), PointerDivert::RawXy);
        // Another client remapped the control. `remap: None` keeps whatever
        // remap the device holds when the write lands.
        assert_eq!(
            armed.redivert_change(reporting(false, true, Some(ControlId(0x0052)))),
            Some(CidReportingChange {
                diverted: Some(true),
                raw_xy: Some(true),
                remap: None,
                ..Default::default()
            })
        );
        // Arming a remapped control wrote its remap; the re-divert still
        // leaves it to the device.
        let remapped = ArmedReporting::pointer(
            CID,
            reporting(false, false, Some(ControlId(0x0056))),
            PointerDivert::Plain,
        );
        assert_eq!(
            remapped
                .redivert_change(reporting(false, false, None))
                .map(|change| change.remap),
            Some(None)
        );
        // Raw XY alone counts too: a raw-XY gesture source without it reports
        // no movement.
        assert!(
            armed
                .redivert_change(reporting(true, false, None))
                .is_some()
        );
    }

    #[test]
    fn a_change_that_kept_the_diversion_needs_no_write() {
        let pointer =
            ArmedReporting::pointer(CID, reporting(false, false, None), PointerDivert::Plain);
        // Another client changed only the remap.
        assert_eq!(
            pointer.redivert_change(reporting(true, false, Some(ControlId(0x0052)))),
            None
        );
        // A keyboard key never writes raw XY, so raw XY does not matter.
        let key = ArmedReporting::keyboard(CID, reporting(false, false, None));
        assert_eq!(key.redivert_change(reporting(true, true, None)), None);
        assert!(key.redivert_change(reporting(false, false, None)).is_some());
    }
}
