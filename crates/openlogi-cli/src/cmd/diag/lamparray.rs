//! `openlogi diag lamparray` — report Logitech's LampArray filter on this host.
//!
//! Logitech ships a Windows USB **lower filter driver**, `logi_lamparray`,
//! installed by `logi_lamparray_usb.inf` and attached to LIGHTSPEED
//! receivers. The filter exists so Windows Dynamic Lighting can drive a
//! device's RGB zones — but the driver package attaches it to receivers for
//! mice that have no RGB at all, and a filter that wedges during a
//! suspend/resume cycle swallows every HID report the mouse sends. The
//! device tree then looks perfectly healthy (Device Manager reports
//! `Status=OK` and no problem code) while the mouse is dead, and only a
//! re-enumeration — unplugging the receiver, or rebooting — brings it back.
//!
//! This command reports what is attached and what follows from it; it
//! changes nothing on the host. The upstream report is
//! <https://github.com/Logitech/cpg-docs/issues/25>, and a repair script
//! that strips the filter lives at
//! <https://github.com/Kirito14IT/logitech-mouse-not-working-after-sleep>.
//!
//! The lower filter and its service registration are Windows constructs.
//! Elsewhere the command reports that the check does not apply.

use anyhow::Result;
use clap::Args;

#[cfg(not(target_os = "windows"))]
mod other;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(not(target_os = "windows"))]
use self::other as platform;
#[cfg(target_os = "windows")]
use self::windows as platform;

/// The USB lower filter driver Logitech's LampArray package installs.
pub const LAMPARRAY_FILTER: &str = "logi_lamparray";

/// The kernel-mode driver behind [`LAMPARRAY_FILTER`].
pub const LAMPARRAY_DRIVER: &str = "logi_lamparray";

/// The user-mode service behind [`LAMPARRAY_FILTER`].
pub const LAMPARRAY_SERVICE: &str = "logi_lamparray_service";

/// The upstream report for the filter defect this command detects.
const UPSTREAM_REPORT: &str = "https://github.com/Logitech/cpg-docs/issues/25";

/// A repair script that strips the filter and stops the recurrence.
const REPAIR_SCRIPT: &str = "https://github.com/Kirito14IT/logitech-mouse-not-working-after-sleep";

/// Arguments for `openlogi diag lamparray` — the check is host-wide, so it
/// takes none.
#[derive(Debug, Args)]
pub struct LamparrayArgs {}

/// One Logitech receiver device instance and the lower filters Windows has
/// attached to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverFilters {
    /// The receiver's product key, e.g. `VID_046D&PID_C53F`.
    pub product_key: String,
    /// The device instance id under the product key, e.g. `6&18811D57&0&1`.
    pub instance: String,
    /// The `LowerFilters` values on that instance.
    pub lower_filters: Vec<String>,
}

/// How a driver or service is registered on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Registration {
    /// Whether a registration exists under
    /// `HKLM\SYSTEM\CurrentControlSet\Services`.
    pub present: bool,
    /// The `Start` value that says how it loads: 1 boot, 2 auto, 3 on demand,
    /// 4 disabled. Absent when the registration exists but the value could
    /// not be read.
    pub start: Option<u32>,
}

/// What the host looks like with respect to Logitech's LampArray stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostState {
    /// Every Logitech receiver device instance found, with its lower filters.
    pub receivers: Vec<ReceiverFilters>,
    /// The `logi_lamparray` kernel driver's registration.
    pub driver: Registration,
    /// The `logi_lamparray_service` registration.
    pub service: Registration,
}

/// The conclusion the report draws from a host state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The check does not apply on this platform.
    NotApplicable,
    /// No Logitech receiver carries a USB lower filter.
    NoFilters,
    /// A receiver carries lower filters, but not Logitech's LampArray one.
    OtherFiltersOnly,
    /// A receiver carries Logitech's `logi_lamparray` lower filter.
    LampArrayAttached,
}

/// Report Logitech's LampArray filter on this host.
///
/// Reports only: it reads the registry and prints, and never writes to the
/// host. The exit status is always success — a host carrying the filter is a
/// finding to report, not a failure of the command.
pub fn run(_args: LamparrayArgs) -> Result<()> {
    let state = platform::collect()?;
    print_report(state.as_ref(), verdict(state.as_ref()));
    Ok(())
}

/// Draw the report's conclusion from a host state.
///
/// `None` means the check does not apply on this platform.
pub fn verdict(state: Option<&HostState>) -> Verdict {
    let Some(state) = state else {
        return Verdict::NotApplicable;
    };
    if state.receivers.iter().any(carries_lamparray_filter) {
        return Verdict::LampArrayAttached;
    }
    if state
        .receivers
        .iter()
        .any(|receiver| !receiver.lower_filters.is_empty())
    {
        return Verdict::OtherFiltersOnly;
    }
    Verdict::NoFilters
}

/// Whether a receiver instance carries Logitech's LampArray lower filter.
fn carries_lamparray_filter(receiver: &ReceiverFilters) -> bool {
    receiver
        .lower_filters
        .iter()
        .any(|filter| filter.eq_ignore_ascii_case(LAMPARRAY_FILTER))
}

/// How a registration reads on one line.
fn registration_line(registration: Registration) -> String {
    if !registration.present {
        return "not registered".to_string();
    }
    match registration.start {
        Some(1) => "registered (Start=1, boot start)".to_string(),
        Some(2) => "registered (Start=2, auto start)".to_string(),
        Some(3) => "registered (Start=3, on demand)".to_string(),
        Some(4) => "registered (Start=4, disabled)".to_string(),
        Some(other) => format!("registered (Start={other})"),
        None => "registered (Start unreadable)".to_string(),
    }
}

/// Print the report for a host state and the conclusion drawn from it.
fn print_report(state: Option<&HostState>, verdict: Verdict) {
    let Some(state) = state else {
        println!("Logitech LampArray stack: not applicable on this platform.");
        println!();
        println!("  The lower filter and its service are Windows constructs;");
        println!("  macOS and Linux have no equivalent to inspect.");
        return;
    };

    println!("Logitech LampArray stack (Windows USB lighting filter)");
    println!();
    println!("  receivers scanned      : {}", state.receivers.len());
    println!(
        "  kernel driver {LAMPARRAY_DRIVER:<20}: {}",
        registration_line(state.driver)
    );
    println!(
        "  user service {LAMPARRAY_SERVICE:<20}: {}",
        registration_line(state.service)
    );
    println!();
    for receiver in &state.receivers {
        println!(
            "  receiver {} : {}",
            receiver.product_key, receiver.instance
        );
        if receiver.lower_filters.is_empty() {
            println!("    lower filters: (none)");
        } else {
            println!("    lower filters: {}", receiver.lower_filters.join(", "));
        }
    }
    println!();

    match verdict {
        Verdict::NotApplicable => {}
        Verdict::NoFilters => {
            println!("VERDICT: no Logitech receiver on this host carries a USB lower filter,");
            println!("so the suspend/resume input-loss defect does not apply here.");
        }
        Verdict::OtherFiltersOnly => {
            println!("VERDICT: a Logitech receiver carries lower filters, but not Logitech's");
            println!("LampArray one - the suspend/resume input-loss defect does not apply here.");
        }
        Verdict::LampArrayAttached => {
            println!("VERDICT: the `{LAMPARRAY_FILTER}` lower filter is attached to a Logitech");
            println!("receiver on this host.");
            println!();
            println!("That filter is a known cause of mouse input loss after suspend/resume on");
            println!("Windows: it can wedge and swallow every HID report the mouse sends, while");
            println!("Device Manager still reports the device as healthy. It is attached to");
            println!(
                "receivers for mice that have no RGB lighting at all, so it buys nothing there."
            );
            println!();
            println!("  upstream report : {UPSTREAM_REPORT}");
            println!("  repair script   : {REPAIR_SCRIPT}");
            println!();
            println!("This command only reports; it changed nothing on this host.");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HostState, LAMPARRAY_FILTER, LamparrayArgs, ReceiverFilters, Registration, Verdict,
        carries_lamparray_filter, registration_line, verdict,
    };

    fn receiver(product_key: &str, instance: &str, filters: &[&str]) -> ReceiverFilters {
        ReceiverFilters {
            product_key: product_key.to_string(),
            instance: instance.to_string(),
            lower_filters: filters.iter().map(|filter| (*filter).to_string()).collect(),
        }
    }

    fn state(receivers: Vec<ReceiverFilters>) -> HostState {
        HostState {
            receivers,
            driver: Registration::default(),
            service: Registration::default(),
        }
    }

    #[test]
    fn a_platform_without_the_check_has_its_own_verdict() {
        assert_eq!(verdict(None), Verdict::NotApplicable);
    }

    #[test]
    fn a_receiver_without_filters_is_clean() {
        let host = state(vec![receiver("VID_046D&PID_C53F", "6&18811D57&0&1", &[])]);
        assert_eq!(verdict(Some(&host)), Verdict::NoFilters);
    }

    #[test]
    fn a_receiver_with_an_unrelated_filter_is_not_the_lamparray_one() {
        let host = state(vec![receiver(
            "VID_046D&PID_C53F",
            "6&18811D57&0&1",
            &["vhf"],
        )]);
        assert_eq!(verdict(Some(&host)), Verdict::OtherFiltersOnly);
    }

    #[test]
    fn the_lamparray_filter_on_any_receiver_is_the_defect() {
        let host = state(vec![
            receiver("VID_046D&PID_C53F", "6&18811D57&0&1", &["vhf"]),
            receiver(
                "VID_046D&PID_C547",
                "7&31912F88&0&4",
                &["vhf", LAMPARRAY_FILTER],
            ),
        ]);
        assert_eq!(verdict(Some(&host)), Verdict::LampArrayAttached);
    }

    /// The filter name is matched case-insensitively: Windows preserves the
    /// spelling the INF wrote, and a different casing is still the same driver.
    #[test]
    fn the_filter_match_ignores_case() {
        let host = state(vec![receiver(
            "VID_046D&PID_C53F",
            "6&18811D57&0&1",
            &["LOGI_LAMPARRAY"],
        )]);
        assert_eq!(verdict(Some(&host)), Verdict::LampArrayAttached);
    }

    #[test]
    fn an_empty_receiver_list_is_clean() {
        assert_eq!(verdict(Some(&state(Vec::new()))), Verdict::NoFilters);
    }

    #[test]
    fn the_filter_is_matched_on_the_receiver_not_on_the_service() {
        let receiver = receiver("VID_046D&PID_C53F", "6&18811D57&0&1", &["vhf"]);
        assert!(!carries_lamparray_filter(&receiver));
    }

    #[test]
    fn an_absent_registration_reads_as_not_registered() {
        assert_eq!(registration_line(Registration::default()), "not registered");
    }

    #[test]
    fn a_registered_service_reads_with_its_start_value() {
        let registration = Registration {
            present: true,
            start: Some(2),
        };
        assert_eq!(
            registration_line(registration),
            "registered (Start=2, auto start)"
        );
    }

    /// The command takes no arguments — it is a host-wide check.
    #[test]
    fn the_command_takes_no_arguments() {
        let _ = LamparrayArgs {};
    }
}
