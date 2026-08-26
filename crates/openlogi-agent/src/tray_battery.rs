//! The tray's battery snapshot: what the tray menu shows when it opens.
//!
//! Menu contents only matter while a menu is on screen, and both platforms
//! have an "about to show" hook, so this is a **pull** surface: the agent's
//! core thread publishes a snapshot as inventory arrives and the UI thread
//! reads it when the user opens the menu. No timer, no idle wakeups — the
//! idle-CPU regression #97 fixed is not worth reintroducing for text nobody is
//! looking at.
//!
//! Strings here are hardcoded English: the agent links no i18n, and its
//! existing tray strings ("Show Main Window", "Quit OpenLogi") are the same.

use std::sync::{OnceLock, RwLock};

use openlogi_core::device::{BatteryStatus, DeviceInventory};

/// One battery row in the tray menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayDevice {
    /// User-facing device name.
    pub name: String,
    /// Reported charge percentage.
    pub percentage: u8,
    /// Charging state at poll time, so the row can say "charging" when the
    /// percentage is not yet meaningful.
    pub status: BatteryStatus,
}

/// The published snapshot. Written by the agent core thread, read by the
/// platform tray thread when its menu opens.
fn cell() -> &'static RwLock<Vec<TrayDevice>> {
    static CELL: OnceLock<RwLock<Vec<TrayDevice>>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(Vec::new()))
}

/// Derive the menu rows from an inventory snapshot.
///
/// Only online devices that actually report a battery get a row; offline and
/// battery-less devices are omitted entirely, so a host with nothing to report
/// renders exactly the menu it renders today.
#[must_use]
pub fn rows(inventories: &[DeviceInventory]) -> Vec<TrayDevice> {
    inventories
        .iter()
        .flat_map(|inventory| inventory.paired.iter())
        .filter(|device| device.online)
        .filter_map(|device| {
            device.battery.as_ref().map(|battery| TrayDevice {
                name: device.display_name(),
                percentage: battery.percentage,
                status: battery.status,
            })
        })
        .collect()
}

/// The row's text, e.g. `"MX Master 3S — 74%"`.
///
/// A device charging while still reporting 0% renders "charging" instead: the
/// MX2S `0x1000` firmware cannot gauge charge under load and a cold start has
/// no cached pre-charge reading, so the 0% is bogus rather than merely stale.
/// Mirrors the GUI's `battery_charging_no_reading`.
///
/// Only the Windows tray consumes this in M1; the macOS menu-bar item gains
/// its own caller in M2, so this is not dead code there for long.
#[must_use]
pub fn label(device: &TrayDevice) -> String {
    let charging_no_reading = matches!(
        device.status,
        BatteryStatus::Charging | BatteryStatus::ChargingSlow
    ) && device.percentage == 0;
    if charging_no_reading {
        format!("{} — charging", device.name)
    } else {
        format!("{} — {}%", device.name, device.percentage)
    }
}

/// Largest tooltip the shell will take, in UTF-16 units.
///
/// `NOTIFYICONDATAW::szTip` is `[u16; 128]`, one of which is the terminator.
pub(crate) const TOOLTIP_LIMIT: usize = 127;

/// Hover text for the tray icon: the app name, then one line per device —
/// deliberately the same wording as the menu rows, so hover and right-click
/// never disagree.
///
/// Truncated with an ellipsis if it would overflow [`TOOLTIP_LIMIT`]; a host
/// with a dozen long device names is rare, but a silently clipped tooltip
/// would look like a bug rather than a limit.
#[must_use]
pub fn tooltip(devices: &[TrayDevice]) -> String {
    let mut tip = String::from("OpenLogi");
    for device in devices {
        tip.push('\n');
        tip.push_str(&label(device));
    }
    if tip.encode_utf16().count() <= TOOLTIP_LIMIT {
        return tip;
    }
    // Rebuild by character so the cut lands on a char boundary, leaving one
    // unit for the ellipsis.
    let mut clipped = String::new();
    let mut units = 0;
    for ch in tip.chars() {
        let width = ch.len_utf16();
        if units + width > TOOLTIP_LIMIT - 1 {
            break;
        }
        clipped.push(ch);
        units += width;
    }
    clipped.push('…');
    clipped
}

/// The last tooltip handed to the shell, so repeats are dropped.
///
/// Seeded with the plain app name because that is what `add_tray_icon`
/// installs from an empty snapshot — the same "model what is actually on
/// screen" rule the glyph filter follows.
///
/// `None` means "we no longer know what the shell is showing", which makes the
/// next snapshot re-send its text. See [`invalidate`].
fn last_tip() -> &'static RwLock<Option<String>> {
    static LAST: OnceLock<RwLock<Option<String>>> = OnceLock::new();
    LAST.get_or_init(|| RwLock::new(Some(String::from("OpenLogi"))))
}

/// Forget the tooltip on screen, so the next snapshot re-sends it even when
/// the text did not change.
///
/// The counterpart to [`crate::tray_glyph::invalidate`], for the same reason:
/// a handoff that never reached the shell must not leave the cache claiming it
/// did, or every identical tick after it is filtered as a repeat.
#[cfg_attr(
    all(not(target_os = "windows"), not(test)),
    expect(
        dead_code,
        reason = "only the Windows tray can fail to hand an update to the shell today"
    )
)]
pub fn invalidate() {
    let mut guard = match last_tip().write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = None;
}

/// Publish a fresh snapshot, returning the new tooltip **only if it changed**.
/// Called from the agent core thread.
pub fn publish(inventories: &[DeviceInventory]) -> Option<String> {
    let next = rows(inventories);
    let tip = tooltip(&next);
    match cell().write() {
        Ok(mut guard) => *guard = next,
        // A poisoned lock means a reader panicked mid-clone. The tray showing
        // stale battery text is not worth taking the agent down over.
        Err(poisoned) => *poisoned.into_inner() = next,
    }

    let mut guard = match last_tip().write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.as_ref() == Some(&tip) {
        return None;
    }
    *guard = Some(tip.clone());
    Some(tip)
}

/// Read the current snapshot. Called from the platform tray thread when its
/// menu is about to open.
#[cfg_attr(
    not(target_os = "windows"),
    expect(
        dead_code,
        reason = "the tray-thread half of this module; only Windows has a caller until the macOS menu-bar item lands"
    )
)]
#[must_use]
pub fn snapshot() -> Vec<TrayDevice> {
    match cell().read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::device::{
        BatteryInfo, BatteryLevel, BatteryStatus, DeviceInventory, DeviceKind, PairedDevice,
        ReceiverInfo,
    };

    use super::{TrayDevice, label, rows};

    #[test]
    fn only_online_devices_that_report_battery_get_a_row() {
        let mut inv = inventory();
        inv[0].paired[1].online = false;
        inv[0].paired[2].battery = None;
        let rows = rows(&inv);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "MX Master 3S");
    }

    #[test]
    fn an_inventory_with_nothing_to_report_yields_no_rows() {
        let mut inv = inventory();
        for device in &mut inv[0].paired {
            device.battery = None;
        }
        assert!(rows(&inv).is_empty());
    }

    #[test]
    fn rows_keep_inventory_order() {
        let rows = rows(&inventory());
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["MX Master 3S", "MX Keys", "Lift"]);
    }

    #[test]
    fn a_row_reads_name_then_percentage() {
        let device = TrayDevice {
            name: "MX Master 3S".to_string(),
            percentage: 74,
            status: BatteryStatus::Discharging,
        };
        assert_eq!(label(&device), "MX Master 3S — 74%");
    }

    #[test]
    fn charging_without_a_reading_says_charging_instead_of_zero() {
        let device = TrayDevice {
            name: "MX Anywhere 2S".to_string(),
            percentage: 0,
            status: BatteryStatus::Charging,
        };
        assert_eq!(label(&device), "MX Anywhere 2S — charging");
    }

    #[test]
    fn charging_with_a_reading_still_shows_the_percentage() {
        let device = TrayDevice {
            name: "MX Keys".to_string(),
            percentage: 40,
            status: BatteryStatus::ChargingSlow,
        };
        assert_eq!(label(&device), "MX Keys — 40%");
    }

    #[test]
    fn a_nameless_device_falls_back_to_kind_and_slot() {
        let mut inv = inventory();
        inv[0].paired[0].codename = None;
        inv[0].paired[0].kind = DeviceKind::Keyboard;
        inv[0].paired[0].slot = 3;
        assert_eq!(rows(&inv)[0].name, "Keyboard (slot 3)");
    }

    #[test]
    fn the_tooltip_is_just_the_app_name_when_nothing_reports_battery() {
        use super::tooltip;
        assert_eq!(tooltip(&[]), "OpenLogi");
    }

    #[test]
    fn the_tooltip_lists_every_device_under_the_app_name() {
        use super::tooltip;
        let tip = tooltip(&rows(&inventory()));
        assert_eq!(
            tip,
            "OpenLogi\nMX Master 3S — 74%\nMX Keys — 55%\nLift — 30%"
        );
    }

    #[test]
    fn the_tooltip_uses_the_same_wording_as_the_menu_rows() {
        use super::tooltip;
        let charging = TrayDevice {
            name: "MX Anywhere 2S".to_string(),
            percentage: 0,
            status: BatteryStatus::Charging,
        };
        assert_eq!(tooltip(&[charging]), "OpenLogi\nMX Anywhere 2S — charging");
    }

    #[test]
    fn a_tooltip_too_long_for_the_win32_buffer_is_truncated_with_an_ellipsis() {
        use super::{TOOLTIP_LIMIT, tooltip};
        let many: Vec<TrayDevice> = (0..12)
            .map(|i| TrayDevice {
                name: format!("A Rather Long Device Name {i}"),
                percentage: 50,
                status: BatteryStatus::Discharging,
            })
            .collect();
        let tip = tooltip(&many);
        let units = tip.encode_utf16().count();
        assert!(
            units <= TOOLTIP_LIMIT,
            "tooltip must fit szTip: {units} units > {TOOLTIP_LIMIT}"
        );
        assert!(tip.ends_with('…'), "a truncated tooltip must say so: {tip}");
    }

    #[test]
    fn publish_reports_only_genuine_tooltip_changes() {
        use super::{invalidate, publish};

        // This test owns the process-global change filter; it is deliberately
        // the only test that touches `publish`, so the shared cell has exactly
        // one writer and the sequence below is deterministic.
        let empty: Vec<DeviceInventory> = Vec::new();
        let listed = inventory();

        // The tray installs the plain app name, which is what an empty
        // snapshot renders — so the first tick has nothing to say.
        assert_eq!(
            publish(&empty),
            None,
            "startup must not re-send the tip the tray just installed"
        );

        let tip = publish(&listed).expect("a device row changes the tooltip");
        assert!(tip.contains("MX Master 3S"));
        assert_eq!(
            publish(&listed),
            None,
            "an identical snapshot must not wake the tray"
        );

        // A handoff that never reached the shell un-records itself, so the
        // next tick re-sends the same text instead of filtering it away.
        invalidate();
        assert_eq!(
            publish(&listed),
            Some(tip),
            "an invalidated cache must re-send even unchanged text"
        );
        assert_eq!(
            publish(&listed),
            None,
            "once re-sent, the filter goes back to dropping repeats"
        );
    }

    fn inventory() -> Vec<DeviceInventory> {
        let named = |slot: u8, codename: &str, percentage: u8| PairedDevice {
            slot,
            codename: Some(codename.to_string()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: Some(BatteryInfo {
                percentage,
                level: BatteryLevel::Good,
                status: BatteryStatus::Discharging,
            }),
            model_info: None,
            capabilities: None,
        };
        vec![DeviceInventory {
            receiver: ReceiverInfo {
                name: "Logi Bolt Receiver".to_string(),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: Some("bolt-1".to_string()),
            },
            paired: vec![
                named(1, "MX Master 3S", 74),
                named(2, "MX Keys", 55),
                named(3, "Lift", 30),
            ],
        }]
    }
}
