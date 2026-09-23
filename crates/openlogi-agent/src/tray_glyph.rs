//! What the tray icon should show, and whether that changed.
//!
//! Unlike the menu rows, the icon must be correct while nobody is looking at
//! it, so it is a **push** surface. The cost of that is contained by
//! [`publish`], which returns `Some` only when the rendered icon actually
//! differs: there are six glyphs keyed off the firmware's discrete level, and
//! real hardware crosses a level a few times a day, so waking the tray thread
//! is a rare event rather than a per-tick one.
//!
//! The filtered state is the whole [`TrayIcon`], **not** just the glyph. The
//! shell keeps whatever icon it was last handed, so "stop sending updates"
//! is not the same as "show the brand mark": switching the setting off has to
//! actively ask for the brand mark back, and switching it on again has to
//! redraw even when the battery itself never moved.

use std::sync::{Mutex, OnceLock};

use openlogi_core::config::TrayIconStyle;
use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus, DeviceInventory};

/// What the notification-area icon should currently be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayIcon {
    /// The OpenLogi brand mark.
    Brand,
    /// A battery indicator for the lowest-charge online device.
    Battery(BatteryGlyph),
}

/// Which Lucide battery glyph the tray should draw.
///
/// Charge state first, then how much charge is left. Whether a reading is low
/// enough to worry about is [`BatteryInfo::needs_attention`], the same call the
/// GUI device card and the low-battery alert make, so the three surfaces cannot
/// disagree about which device is in trouble. Above that line the tray is free
/// to be finer-grained than the card's single "fine" tone, and draws the
/// firmware's own bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryGlyph {
    /// On power.
    Charging,
    /// At or near full charge.
    Full,
    /// Comfortable middle range.
    Medium,
    /// Running low.
    Low,
    /// Critical, or the firmware reported a charging fault.
    Warning,
    /// The firmware did not report a usable level.
    Unknown,
}

#[cfg_attr(
    not(target_os = "windows"),
    expect(
        dead_code,
        reason = "the index round-trip exists for the Windows tray's WPARAM; only Windows has a caller until the macOS menu-bar item lands"
    )
)]
impl BatteryGlyph {
    /// Every glyph, for exhaustive iteration in tests and caches.
    pub const ALL: [Self; 6] = [
        Self::Charging,
        Self::Full,
        Self::Medium,
        Self::Low,
        Self::Warning,
        Self::Unknown,
    ];

    /// Compact index, so the Windows tray can round-trip a glyph through a
    /// `WPARAM` without inventing a second encoding.
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            Self::Charging => 0,
            Self::Full => 1,
            Self::Medium => 2,
            Self::Low => 3,
            Self::Warning => 4,
            Self::Unknown => 5,
        }
    }

    /// Inverse of [`Self::index`]. `None` for a value that never came from it.
    #[must_use]
    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    /// The vendored Lucide file this glyph renders from.
    #[must_use]
    pub fn asset(self) -> &'static str {
        match self {
            Self::Charging => include_str!("../assets/lucide/battery-charging.svg"),
            Self::Full => include_str!("../assets/lucide/battery-full.svg"),
            Self::Medium => include_str!("../assets/lucide/battery-medium.svg"),
            Self::Low => include_str!("../assets/lucide/battery-low.svg"),
            Self::Warning => include_str!("../assets/lucide/battery-warning.svg"),
            Self::Unknown => include_str!("../assets/lucide/battery.svg"),
        }
    }
}

/// The icon currently on screen, so repeats are dropped.
///
/// Seeded with [`TrayIcon::Brand`] because that is what the tray installs at
/// startup — seeding it "unknown" would make the first tick re-send the brand
/// mark the shell is already showing, and log a change that never happened.
///
/// `None` means "we no longer know what the shell is showing", which makes the
/// next snapshot re-send whatever it computes. See [`invalidate`].
fn last() -> &'static Mutex<Option<TrayIcon>> {
    static LAST: OnceLock<Mutex<Option<TrayIcon>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(Some(TrayIcon::Brand)))
}

/// Forget what is on screen, so the next snapshot re-sends its icon even when
/// nothing about the batteries changed.
///
/// This cache models what the shell **is showing**, not what we last decided —
/// so a handoff that never reached the shell has to un-record itself. Without
/// it a dropped update leaves the icon stale until the next genuine glyph
/// transition, because every identical tick in between is filtered as a repeat.
#[cfg_attr(
    all(not(target_os = "windows"), not(test)),
    expect(
        dead_code,
        reason = "only the Windows tray can fail to hand an update to the shell today"
    )
)]
pub fn invalidate() {
    let mut guard = match last().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = None;
}

/// The glyph state for a snapshot, or `None` when no online device reports a
/// battery.
///
/// Selection is **lowest charge wins**: the tray icon is a warning surface, so
/// it shows the device closest to dying. Ties keep inventory order. Every
/// device is still listed in the menu rows, so nothing is hidden by this.
#[must_use]
pub fn state_for(inventories: &[DeviceInventory]) -> Option<BatteryGlyph> {
    inventories
        .iter()
        .flat_map(|inventory| inventory.paired.iter())
        .filter(|device| device.online)
        .filter_map(|device| device.battery.as_ref())
        // Selection is still by percentage — it is the only totally ordered
        // field, and "closest to dying" is the point. `min_by_key` keeps the
        // *first* minimum, which is the inventory-order tie-break promised.
        .min_by_key(|battery| battery.percentage)
        .map(glyph_for)
}

/// The glyph for one battery reading.
///
/// The warning states are decided by the same rules the GUI device card uses —
/// [`BatteryLevel::Critical`] for the urgent one, [`BatteryInfo::needs_attention`]
/// for the merely low one — so the tray and the card never disagree about
/// whether a device is in trouble.
fn glyph_for(battery: &BatteryInfo) -> BatteryGlyph {
    match battery.status {
        BatteryStatus::Charging | BatteryStatus::ChargingSlow => BatteryGlyph::Charging,
        BatteryStatus::Full => BatteryGlyph::Full,
        BatteryStatus::Error => BatteryGlyph::Warning,
        BatteryStatus::Discharging | BatteryStatus::Unknown => {
            if battery.level == BatteryLevel::Critical {
                BatteryGlyph::Warning
            } else if battery.needs_attention() {
                BatteryGlyph::Low
            } else {
                match battery.level {
                    BatteryLevel::Full => BatteryGlyph::Full,
                    BatteryLevel::Unknown => BatteryGlyph::Unknown,
                    // A firmware `Low` this far up the scale is the `0x1000`
                    // and `0x1001` bucket being eager — it calls everything
                    // from 20 % to 49 % `Low`. Above the attention threshold
                    // that is a comfortable charge, and the alert declines to
                    // fire on it for the same reason. `Critical` cannot reach
                    // here at all.
                    BatteryLevel::Critical | BatteryLevel::Low | BatteryLevel::Good => {
                        BatteryGlyph::Medium
                    }
                }
            }
        }
    }
}

/// The icon this snapshot and setting call for.
///
/// Falls back to [`TrayIcon::Brand`] whenever the battery style is off *or*
/// no online device reports a battery — there is no meaningful battery to
/// draw in either case.
#[must_use]
pub fn desired(style: TrayIconStyle, inventories: &[DeviceInventory]) -> TrayIcon {
    if style != TrayIconStyle::Battery {
        return TrayIcon::Brand;
    }
    state_for(inventories).map_or(TrayIcon::Brand, TrayIcon::Battery)
}

/// Compute the desired icon and report it **only if it changed** since the
/// last call. Called from the agent core thread on every inventory snapshot;
/// a `Some` return is the signal to wake the platform tray thread.
pub fn publish(style: TrayIconStyle, inventories: &[DeviceInventory]) -> Option<TrayIcon> {
    let next = desired(style, inventories);
    let mut guard = match last().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if *guard == Some(next) {
        return None;
    }
    *guard = Some(next);
    Some(next)
}

#[cfg(test)]
mod tests {
    use openlogi_core::device::{
        BatteryInfo, BatteryLevel, BatteryStatus, DeviceInventory, DeviceKind, PairedDevice,
        ReceiverInfo,
    };

    use super::{BatteryGlyph, state_for};

    #[test]
    fn the_glyph_mirrors_the_gui_device_card_mapping() {
        // The card's rule, from `openlogi-desktop/src/ui/battery.rs`: the
        // firmware's `Critical` is the urgent state, `needs_attention` is the
        // low one, and everything else is fine. The tray subdivides "fine"
        // into three glyphs the card has no equivalent for, but it must not
        // call a device low that the card calls fine — or the reverse. That
        // is the drift this guards against.
        let cases = [
            (
                BatteryStatus::Charging,
                BatteryLevel::Low,
                15,
                BatteryGlyph::Charging,
            ),
            (
                BatteryStatus::ChargingSlow,
                BatteryLevel::Critical,
                5,
                BatteryGlyph::Charging,
            ),
            (
                BatteryStatus::Full,
                BatteryLevel::Unknown,
                100,
                BatteryGlyph::Full,
            ),
            (
                BatteryStatus::Error,
                BatteryLevel::Good,
                60,
                BatteryGlyph::Warning,
            ),
            (
                BatteryStatus::Discharging,
                BatteryLevel::Critical,
                8,
                BatteryGlyph::Warning,
            ),
            (
                BatteryStatus::Discharging,
                BatteryLevel::Low,
                15,
                BatteryGlyph::Low,
            ),
            // The `0x1000` bucket calls 20-49 % `Low`; above the attention
            // threshold the card draws that as fine, so the tray does too.
            (
                BatteryStatus::Discharging,
                BatteryLevel::Low,
                35,
                BatteryGlyph::Medium,
            ),
            // And the converse: a comfortable bucket does not outrank a
            // reading the card is already warning about.
            (
                BatteryStatus::Discharging,
                BatteryLevel::Good,
                12,
                BatteryGlyph::Low,
            ),
            (
                BatteryStatus::Discharging,
                BatteryLevel::Good,
                60,
                BatteryGlyph::Medium,
            ),
            (
                BatteryStatus::Discharging,
                BatteryLevel::Full,
                95,
                BatteryGlyph::Full,
            ),
            (
                BatteryStatus::Discharging,
                BatteryLevel::Unknown,
                60,
                BatteryGlyph::Unknown,
            ),
            (
                BatteryStatus::Discharging,
                BatteryLevel::Unknown,
                12,
                BatteryGlyph::Low,
            ),
            (
                BatteryStatus::Unknown,
                BatteryLevel::Good,
                60,
                BatteryGlyph::Medium,
            ),
        ];
        for (status, level, percentage, expected) in cases {
            let inv = inventory(&[(1, percentage, level, status)]);
            assert_eq!(
                state_for(&inv),
                Some(expected),
                "{status:?} + {level:?} at {percentage}% should map to {expected:?}"
            );
        }
    }

    /// The tray's low state and the card's are the same predicate, not two
    /// tables that happen to agree today.
    #[test]
    fn the_low_glyph_tracks_the_shared_attention_threshold() {
        for percentage in 0..=100u8 {
            let battery = BatteryInfo {
                percentage,
                level: BatteryLevel::Good,
                status: BatteryStatus::Discharging,
            };
            let inv = inventory(&[(
                1,
                percentage,
                BatteryLevel::Good,
                BatteryStatus::Discharging,
            )]);
            let low = state_for(&inv) == Some(BatteryGlyph::Low);
            assert_eq!(
                low,
                battery.needs_attention(),
                "{percentage}% must draw the low glyph exactly when the card warns"
            );
        }
    }

    #[test]
    fn charge_state_wins_over_the_level() {
        let inv = inventory(&[(1, 5, BatteryLevel::Critical, BatteryStatus::Charging)]);
        assert_eq!(
            state_for(&inv),
            Some(BatteryGlyph::Charging),
            "a critical device on power is charging, not a warning"
        );
    }

    #[test]
    fn the_lowest_battery_device_owns_the_glyph() {
        let inv = inventory(&[
            (1, 74, BatteryLevel::Good, BatteryStatus::Discharging),
            (2, 12, BatteryLevel::Critical, BatteryStatus::Discharging),
        ]);
        assert_eq!(state_for(&inv), Some(BatteryGlyph::Warning));
    }

    #[test]
    fn ties_break_on_inventory_order() {
        let inv = inventory(&[
            (1, 20, BatteryLevel::Critical, BatteryStatus::Discharging),
            (2, 20, BatteryLevel::Good, BatteryStatus::Discharging),
        ]);
        assert_eq!(
            state_for(&inv),
            Some(BatteryGlyph::Warning),
            "the first device at the tied percentage wins"
        );
    }

    #[test]
    fn offline_devices_are_not_candidates() {
        let mut inv = inventory(&[
            (1, 74, BatteryLevel::Good, BatteryStatus::Discharging),
            (2, 12, BatteryLevel::Critical, BatteryStatus::Discharging),
        ]);
        inv[0].paired[1].online = false;
        assert_eq!(state_for(&inv), Some(BatteryGlyph::Medium));
    }

    #[test]
    fn nothing_to_report_means_no_glyph() {
        assert_eq!(state_for(&[]), None);

        let mut inv = inventory(&[(1, 74, BatteryLevel::Good, BatteryStatus::Discharging)]);
        inv[0].paired[0].battery = None;
        assert_eq!(
            state_for(&inv),
            None,
            "a device with no battery is not a candidate"
        );
    }

    #[test]
    fn every_glyph_has_a_vendored_recolourable_svg() {
        for glyph in [
            BatteryGlyph::Charging,
            BatteryGlyph::Full,
            BatteryGlyph::Medium,
            BatteryGlyph::Low,
            BatteryGlyph::Warning,
            BatteryGlyph::Unknown,
        ] {
            let svg = glyph.asset();
            assert!(svg.contains("<svg"), "{glyph:?} asset is not an SVG");
            assert!(
                svg.contains("currentColor"),
                "{glyph:?} must be recolourable by stroke substitution"
            );
        }
    }

    #[test]
    fn publish_reports_only_genuine_changes() {
        use super::{TrayIcon, publish};
        use openlogi_core::config::TrayIconStyle::{Battery, Brand};

        // This test owns the process-global change filter; it is deliberately
        // the only test that touches `publish`, so the shared cell has exactly
        // one writer and the sequence below is deterministic.
        let good = inventory(&[(1, 74, BatteryLevel::Good, BatteryStatus::Discharging)]);
        let still_good = inventory(&[(1, 73, BatteryLevel::Good, BatteryStatus::Discharging)]);
        let low = inventory(&[(1, 20, BatteryLevel::Low, BatteryStatus::Discharging)]);

        // First call of the process: the tray already installed the brand
        // mark, so a brand-styled tick has nothing to say.
        assert_eq!(
            publish(Brand, &good),
            None,
            "startup must not re-send the brand mark the tray just installed"
        );

        assert_eq!(
            publish(Battery, &good),
            Some(TrayIcon::Battery(BatteryGlyph::Medium))
        );
        assert_eq!(
            publish(Battery, &good),
            None,
            "an identical snapshot must not wake the tray"
        );
        assert_eq!(
            publish(Battery, &still_good),
            None,
            "a percentage change that keeps the same level must not wake the tray"
        );
        assert_eq!(
            publish(Battery, &low),
            Some(TrayIcon::Battery(BatteryGlyph::Low))
        );

        // Turning the setting off must actively restore the brand mark: the
        // shell keeps whatever icon it was last handed, so merely stopping
        // updates would strand a battery glyph on screen forever.
        assert_eq!(
            publish(Brand, &low),
            Some(TrayIcon::Brand),
            "disabling the battery glyph must ask for the brand mark back"
        );
        assert_eq!(
            publish(Brand, &low),
            None,
            "the brand mark must not be re-sent every tick"
        );
        assert_eq!(
            publish(Brand, &good),
            None,
            "battery changes are irrelevant while the brand mark is shown"
        );

        // Re-enabling must redraw even though the battery state itself has
        // not moved since the last time the glyph was shown.
        assert_eq!(
            publish(Battery, &good),
            Some(TrayIcon::Battery(BatteryGlyph::Medium)),
            "re-enabling must redraw the glyph, not be filtered as unchanged"
        );

        // A handoff that never reached the shell un-records itself, so the very
        // next tick re-sends the same icon rather than filtering it as a
        // repeat and leaving the shell showing something else.
        super::invalidate();
        assert_eq!(
            publish(Battery, &good),
            Some(TrayIcon::Battery(BatteryGlyph::Medium)),
            "an invalidated cache must re-send even an unchanged icon"
        );
        assert_eq!(
            publish(Battery, &good),
            None,
            "once re-sent, the filter goes back to dropping repeats"
        );
    }

    #[test]
    fn a_host_with_no_battery_device_shows_the_brand_mark() {
        use super::{TrayIcon, desired};
        use openlogi_core::config::TrayIconStyle::Battery;

        assert_eq!(
            desired(Battery, &[]),
            TrayIcon::Brand,
            "with nothing to report there is no battery to draw"
        );
    }

    /// Tuple: (slot, percentage, level, status).
    fn inventory(devices: &[(u8, u8, BatteryLevel, BatteryStatus)]) -> Vec<DeviceInventory> {
        vec![DeviceInventory {
            receiver: ReceiverInfo {
                name: "Logi Bolt Receiver".to_string(),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: Some("bolt-1".to_string()),
            },
            paired: devices
                .iter()
                .map(|&(slot, percentage, level, status)| PairedDevice {
                    slot,
                    codename: Some(format!("device-{slot}")),
                    wpid: None,
                    kind: DeviceKind::Mouse,
                    online: true,
                    battery: Some(BatteryInfo {
                        percentage,
                        level,
                        status,
                    }),
                    model_info: None,
                    capabilities: None,
                })
                .collect(),
        }]
    }
}
