//! The notices the overlay shows: when the agent posts one, and how long it
//! stays.
//!
//! The agent owns a notice's whole life. It posts the notice, and the notice
//! ends when its time is up, so an overlay that starts or reconnects shows only
//! what is still current and never a toast it missed. Nothing runs a timer:
//! the long-poll that waits for a change also waits for the next notice to end,
//! and every read and every post first drops the notices whose time is up.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus};
use openlogi_ipc::{Generation, Notice, NoticeKind, NoticeObservation, OBSERVE_HOLD};
use tokio::sync::watch;
use tokio::time::Instant;

/// How long a notice shows.
pub const NOTICE_LIFETIME: Duration = Duration::from_secs(8);

/// A charge at or above which a battery that was low counts as charged again.
const RECHARGED_PERCENT: u8 = 30;

struct Posted {
    notice: Notice,
    ends: Instant,
}

/// What a republish does with the locale the observation carries.
enum Locale {
    /// Keep the one the last post set: an ending notice says nothing about it.
    Keep,
    /// Set the one a new post was made in (`None` follows the overlay host).
    Set(Option<String>),
}

/// The notices the overlay should be showing, and the cell it observes.
pub struct NoticeBoard {
    next_id: AtomicU64,
    posted: Mutex<Vec<Posted>>,
    published: watch::Sender<NoticeObservation>,
}

impl Default for NoticeBoard {
    fn default() -> Self {
        // Generation 1: 0 is the observer's "seen nothing" sentinel.
        let (published, _) = watch::channel(NoticeObservation {
            generation: 1,
            notices: Vec::new(),
            language: None,
        });
        Self {
            next_id: AtomicU64::new(1),
            posted: Mutex::new(Vec::new()),
            published,
        }
    }
}

impl NoticeBoard {
    /// Show `kind` for [`NOTICE_LIFETIME`], in the UI locale `language`
    /// (`None` follows the overlay host's system locale).
    pub fn post(&self, kind: NoticeKind, language: Option<String>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let now = Instant::now();
        let mut posted = self.posted();
        // No overlay may be observing to end them, so a post drops what has
        // ended too: the list never grows past the notices still current.
        posted.retain(|posted| posted.ends > now);
        posted.push(Posted {
            notice: Notice { id, kind },
            ends: now + NOTICE_LIFETIME,
        });
        self.publish(&posted, Locale::Set(language));
    }

    /// Serve one [`Agent::observe_notices`](openlogi_ipc::Agent::observe_notices):
    /// answer once the notices differ from generation `since`, including when
    /// one ends, or with the unchanged notices after [`OBSERVE_HOLD`].
    pub async fn observe(&self, since: Generation) -> NoticeObservation {
        let hold_ends = Instant::now() + OBSERVE_HOLD;
        let mut rx = self.published.subscribe();
        loop {
            let now = Instant::now();
            let next_end = self.end_due(now);
            {
                let observed = rx.borrow_and_update();
                if observed.generation != since || now >= hold_ends {
                    return observed.clone();
                }
            }
            let wake = next_end.map_or(hold_ends, |end| end.min(hold_ends));
            // A change, a notice's end, or the end of the hold: look again.
            // The sender lives as long as `self`, so `changed` cannot fail.
            let _ = tokio::time::timeout_at(wake, rx.changed()).await;
        }
    }

    /// Drop the notices whose time is up at `now`, and say when the next of
    /// the rest ends.
    fn end_due(&self, now: Instant) -> Option<Instant> {
        let mut posted = self.posted();
        let before = posted.len();
        posted.retain(|posted| posted.ends > now);
        if posted.len() != before {
            self.publish(&posted, Locale::Keep);
        }
        posted.iter().map(|posted| posted.ends).min()
    }

    /// Republish the notices with the `locale` given.
    ///
    /// Every caller changes the list: a post adds a notice with a new id, and
    /// an end republishes only when a notice has gone.
    fn publish(&self, posted: &[Posted], locale: Locale) {
        let notices: Vec<Notice> = posted.iter().map(|posted| posted.notice.clone()).collect();
        self.published.send_modify(|observed| {
            if let Locale::Set(language) = locale {
                observed.language = language;
            }
            observed.notices = notices;
            observed.generation += 1;
        });
    }

    fn posted(&self) -> MutexGuard<'_, Vec<Posted>> {
        // A panic mid-update leaves at worst a notice that ends late.
        self.posted.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Which devices have had their low battery announced.
///
/// A device is announced once per discharge: when its battery first needs
/// attention ([`BatteryInfo::needs_attention`]), and again only after it has
/// been charged. A reading that wobbles around the threshold is not a new
/// discharge.
///
/// While the user has the notices off, the watch still takes in every
/// reading with [`Self::observe_muted`], so a recharge in that time still
/// ends the device's discharge.
#[derive(Debug, Default)]
pub struct LowBatteryWatch {
    announced: HashSet<String>,
}

impl LowBatteryWatch {
    /// Take in `device`'s latest reading. Answers whether to announce its low
    /// battery now.
    pub fn observe(&mut self, device: &str, battery: &BatteryInfo) -> bool {
        if battery.needs_attention() {
            return self.announced.insert(device.to_owned());
        }
        self.observe_muted(device, battery);
        false
    }

    /// Take in `device`'s latest reading while the user has the notices off.
    /// A recharge still ends its discharge. A low reading is not marked as
    /// announced, so the first low reading after the notices are back on
    /// announces it. A reading that needs attention is never a recharge.
    pub fn observe_muted(&mut self, device: &str, battery: &BatteryInfo) {
        if recharged(battery) {
            self.announced.remove(device);
        }
    }
}

/// Whether a battery has been charged since it was low: it is charging now,
/// it reads [`RECHARGED_PERCENT`] or more, or, without a percentage, its level
/// is good or full again.
fn recharged(battery: &BatteryInfo) -> bool {
    let charging = matches!(
        battery.status,
        BatteryStatus::Charging | BatteryStatus::ChargingSlow | BatteryStatus::Full
    );
    charging
        || battery.percentage.map_or_else(
            || matches!(battery.level, BatteryLevel::Good | BatteryLevel::Full),
            |charge| charge >= RECHARGED_PERCENT,
        )
}

#[cfg(test)]
mod tests;
