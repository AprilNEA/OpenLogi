//! Manual smoke-test for `Action::execute`.
//!
//! Parses action names from arguments, waits for the configured delay, then
//! fires each one in order. The delay lets you focus the target window before
//! injection begins.
//!
//! # Usage
//!
//! ```text
//! cargo build --example inject_action -p openlogi-core
//! sudo ./target/debug/examples/inject_action [--delay <secs>] <Action> [<Action> ...]
//! ```
//!
//! # Examples
//!
//! ```text
//! # Open a text editor, select some text, then:
//! sudo ./target/debug/examples/inject_action --delay 3 Copy
//!
//! # Fire several actions back-to-back (0.5 s between each):
//! sudo ./target/debug/examples/inject_action --delay 2 --between 500 VolumeUp VolumeDown PlayPause
//!
//! # Inject a scroll sequence:
//! sudo ./target/debug/examples/inject_action ScrollDown ScrollDown ScrollDown ScrollUp
//! ```
//!
//! # Available actions
//!
//! LeftClick RightClick MiddleClick
//! Copy Paste Cut Undo Redo SelectAll Find Save
//! BrowserBack BrowserForward NewTab CloseTab ReopenTab NextTab PrevTab ReloadPage
//! MissionControl AppExpose PreviousDesktop NextDesktop ShowDesktop LaunchpadShow
//! LockScreen Screenshot
//! PlayPause NextTrack PrevTrack VolumeUp VolumeDown MuteVolume
//! CycleDpiPresets ToggleSmartShift
//! ScrollUp ScrollDown HorizontalScrollLeft HorizontalScrollRight

use std::time::Duration;

#[cfg(target_os = "linux")]
use openlogi_core::binding::action_device_path;
use openlogi_core::binding::{Action, KeyCombo};

fn parse_action(s: &str) -> Result<Action, String> {
    Ok(match s {
        "LeftClick" => Action::LeftClick,
        "RightClick" => Action::RightClick,
        "MiddleClick" => Action::MiddleClick,
        "Copy" => Action::Copy,
        "Paste" => Action::Paste,
        "Cut" => Action::Cut,
        "Undo" => Action::Undo,
        "Redo" => Action::Redo,
        "SelectAll" => Action::SelectAll,
        "Find" => Action::Find,
        "Save" => Action::Save,
        "BrowserBack" => Action::BrowserBack,
        "BrowserForward" => Action::BrowserForward,
        "NewTab" => Action::NewTab,
        "CloseTab" => Action::CloseTab,
        "ReopenTab" => Action::ReopenTab,
        "NextTab" => Action::NextTab,
        "PrevTab" => Action::PrevTab,
        "ReloadPage" => Action::ReloadPage,
        "MissionControl" => Action::MissionControl,
        "AppExpose" => Action::AppExpose,
        "PreviousDesktop" => Action::PreviousDesktop,
        "NextDesktop" => Action::NextDesktop,
        "ShowDesktop" => Action::ShowDesktop,
        "LaunchpadShow" => Action::LaunchpadShow,
        "LockScreen" => Action::LockScreen,
        "Screenshot" => Action::Screenshot,
        "PlayPause" => Action::PlayPause,
        "NextTrack" => Action::NextTrack,
        "PrevTrack" => Action::PrevTrack,
        "VolumeUp" => Action::VolumeUp,
        "VolumeDown" => Action::VolumeDown,
        "MuteVolume" => Action::MuteVolume,
        "CycleDpiPresets" => Action::CycleDpiPresets,
        "ToggleSmartShift" => Action::ToggleSmartShift,
        "ScrollUp" => Action::ScrollUp,
        "ScrollDown" => Action::ScrollDown,
        "HorizontalScrollLeft" => Action::HorizontalScrollLeft,
        "HorizontalScrollRight" => Action::HorizontalScrollRight,
        other if other.starts_with("CustomShortcut:") => {
            // Format: CustomShortcut:<modifiers>:<key_code>
            // modifiers is a hex byte (e.g. 0x05 for Ctrl+Shift), key_code is a hex u16.
            // Example: CustomShortcut:0x04:0x08 → Ctrl+C on macOS layout
            let parts: Vec<&str> = other.splitn(3, ':').collect();
            if parts.len() != 3 {
                return Err(
                    "CustomShortcut format: CustomShortcut:<mod_hex>:<key_hex> (e.g. CustomShortcut:0x01:0x08)".to_string()
                );
            }
            let modifiers = parse_hex_u8(parts[1])
                .ok_or_else(|| format!("invalid modifier byte: {}", parts[1]))?;
            let key_code =
                parse_hex_u16(parts[2]).ok_or_else(|| format!("invalid key code: {}", parts[2]))?;
            Action::CustomShortcut(KeyCombo {
                modifiers,
                key_code,
                display: String::new(),
            })
        }
        _ => return Err(format!("unknown action: {s}")),
    })
}

fn strip_hex_prefix(s: &str) -> &str {
    s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s)
}

fn parse_hex_u8(s: &str) -> Option<u8> {
    u8::from_str_radix(strip_hex_prefix(s), 16).ok()
}

fn parse_hex_u16(s: &str) -> Option<u16> {
    u16::from_str_radix(strip_hex_prefix(s), 16).ok()
}

fn main() {
    let mut args = std::env::args().skip(1);

    let mut initial_delay_secs: f64 = 2.0;
    let mut between_ms: u64 = 200;
    let mut actions: Vec<Action> = Vec::new();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--delay" => {
                let val = args.next().unwrap_or_else(|| {
                    eprintln!("--delay requires a value");
                    std::process::exit(1);
                });
                initial_delay_secs = val.parse().unwrap_or_else(|_| {
                    eprintln!("--delay: expected a number, got {val}");
                    std::process::exit(1);
                });
            }
            "--between" => {
                let val = args.next().unwrap_or_else(|| {
                    eprintln!("--between requires a value (milliseconds)");
                    std::process::exit(1);
                });
                between_ms = val.parse().unwrap_or_else(|_| {
                    eprintln!("--between: expected a number, got {val}");
                    std::process::exit(1);
                });
            }
            "--help" | "-h" => {
                print_usage();
                return;
            }
            name => match parse_action(name) {
                Ok(action) => actions.push(action),
                Err(e) => {
                    eprintln!("error: {e}");
                    eprintln!("Run with --help for the list of available actions.");
                    std::process::exit(1);
                }
            },
        }
    }

    if actions.is_empty() {
        eprintln!("error: no actions specified");
        print_usage();
        std::process::exit(1);
    }

    // On Linux, initialise the uinput device eagerly so we can print its node
    // path before the countdown — giving time to attach evtest in another terminal.
    #[cfg(target_os = "linux")]
    match action_device_path() {
        Some(path) => {
            println!("uinput device: {}", path.display());
            println!("  To monitor raw events, open another terminal and run:");
            println!("  sudo evtest {}", path.display());
        }
        None => {
            eprintln!("warning: could not find uinput device node (check /dev/uinput permissions)");
        }
    }

    let delay = Duration::from_secs_f64(initial_delay_secs);
    println!(
        "Injecting {} action(s) in {:.1}s — focus the target window now...",
        actions.len(),
        initial_delay_secs
    );
    std::thread::sleep(delay);

    let between = Duration::from_millis(between_ms);
    for (i, action) in actions.iter().enumerate() {
        println!("  → {}", action.label());
        action.execute();
        if i + 1 < actions.len() {
            std::thread::sleep(between);
        }
    }
    println!("Done.");
}

fn print_usage() {
    eprintln!(
        "Usage: inject_action [--delay <secs>] [--between <ms>] <Action> [<Action> ...]\n\
         \n\
         Options:\n\
           --delay <secs>    seconds to wait before first injection (default: 2)\n\
           --between <ms>    milliseconds between actions (default: 200)\n\
         \n\
         Actions: LeftClick RightClick MiddleClick\n\
                  Copy Paste Cut Undo Redo SelectAll Find Save\n\
                  BrowserBack BrowserForward NewTab CloseTab ReopenTab\n\
                  NextTab PrevTab ReloadPage\n\
                  MissionControl AppExpose PreviousDesktop NextDesktop\n\
                  ShowDesktop LaunchpadShow\n\
                  LockScreen Screenshot\n\
                  PlayPause NextTrack PrevTrack VolumeUp VolumeDown MuteVolume\n\
                  CycleDpiPresets ToggleSmartShift\n\
                  ScrollUp ScrollDown HorizontalScrollLeft HorizontalScrollRight\n\
                  CustomShortcut:<mod_hex>:<key_hex>\n\
         \n\
         CustomShortcut modifier bits: 0x01=Cmd/Ctrl 0x02=Shift 0x04=Ctrl 0x08=Option/Alt\n\
         CustomShortcut key_hex: macOS kVK_* code (e.g. 0x08=C, 0x09=V, 0x7E=Up)\n\
         \n\
         Examples:\n\
           inject_action --delay 3 Copy\n\
           inject_action --delay 2 --between 500 VolumeUp VolumeDown PlayPause\n\
           inject_action ScrollDown ScrollDown ScrollDown\n\
           inject_action CustomShortcut:0x01:0x08   # Ctrl+C"
    );
}
