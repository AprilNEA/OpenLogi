//! Manual smoke-test for `openlogi_inject::execute`.
//!
//! Parses action names from arguments, waits for the configured delay, then
//! fires each one in order. The delay lets you focus the target window before
//! injection begins.
//!
//! # Usage
//!
//! ```text
//! cargo build --example inject_action -p openlogi-inject
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
//!
//! # Hold-mode pan (pixel scroll with phase) and zoom (pinch / Ctrl+wheel):
//! sudo ./target/debug/examples/inject_action --delay 2 PanBegin Pan:8:6 Pan:8:6 PanEnd
//! sudo ./target/debug/examples/inject_action --delay 2 Zoom:0.05 Zoom:0.05 ZoomEnd
//! ```
//!
//! # Available actions
//!
//! LeftClick RightClick MiddleClick MouseBack MouseForward
//! Copy Paste Cut Undo Redo SelectAll Find Save
//! BrowserBack BrowserForward NewTab CloseTab ReopenTab NextTab PrevTab ReloadPage
//! MissionControl AppExpose PreviousDesktop NextDesktop ShowDesktop LaunchpadShow
//! LockScreen Screenshot
//! PlayPause NextTrack PrevTrack VolumeUp VolumeDown MuteVolume
//! CycleDpiPresets ToggleSmartShift
//! ScrollUp ScrollDown HorizontalScrollLeft HorizontalScrollRight
//! PanBegin Pan:<dx>:<dy> PanEnd Zoom:<amount> ZoomEnd FlushGestures

use std::time::Duration;

use serde::Deserialize;
use serde::de::IntoDeserializer;

use openlogi_core::binding::{Action, KeyCombo};
#[cfg(target_os = "linux")]
use openlogi_inject::action_device_path;

enum Step {
    Action(Action),
    PanBegin,
    Pan { dx: f32, dy: f32 },
    PanEnd,
    Zoom(f32),
    ZoomEnd,
    FlushGestures,
}

fn parse_step(s: &str) -> Result<Step, String> {
    match s {
        "PanBegin" => return Ok(Step::PanBegin),
        "PanEnd" => return Ok(Step::PanEnd),
        "ZoomEnd" => return Ok(Step::ZoomEnd),
        "FlushGestures" => return Ok(Step::FlushGestures),
        _ => {}
    }
    if let Some(rest) = s.strip_prefix("Pan:") {
        let mut parts = rest.split(':');
        let dx = parts
            .next()
            .ok_or_else(|| format!("Pan: expected Pan:<dx>:<dy>, got {s}"))?;
        let dy = parts
            .next()
            .ok_or_else(|| format!("Pan: expected Pan:<dx>:<dy>, got {s}"))?;
        if parts.next().is_some() {
            return Err(format!("Pan: expected Pan:<dx>:<dy>, got {s}"));
        }
        let dx: f32 = dx
            .parse()
            .map_err(|_| format!("Pan: dx is not a number: {dx}"))?;
        let dy: f32 = dy
            .parse()
            .map_err(|_| format!("Pan: dy is not a number: {dy}"))?;
        return Ok(Step::Pan { dx, dy });
    }
    if let Some(rest) = s.strip_prefix("Zoom:") {
        let amount: f32 = rest
            .parse()
            .map_err(|_| format!("Zoom: amount is not a number: {rest}"))?;
        return Ok(Step::Zoom(amount));
    }
    parse_action(s).map(Step::Action)
}

fn parse_action(s: &str) -> Result<Action, String> {
    // `CustomShortcut` has its own CLI syntax (serde expects a table for the
    // tuple variant), so parse it by hand.
    if let Some(shortcut) = s.strip_prefix("CustomShortcut:") {
        return shortcut
            .parse::<KeyCombo>()
            .map(Action::CustomShortcut)
            .map_err(|error| error.to_string());
    }

    // Every other variant is a serde unit variant that deserializes straight
    // from its name, so this list never drifts out of sync with the `Action`
    // enum the way a hand-written match would.
    Action::deserialize(s.into_deserializer())
        .map_err(|_: serde::de::value::Error| format!("unknown action: {s}"))
}

fn main() {
    let mut args = std::env::args().skip(1);

    let mut initial_delay_secs: f64 = 2.0;
    let mut between_ms: u64 = 200;
    let mut verbose = false;
    let mut steps: Vec<Step> = Vec::new();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--verbose" | "-v" => {
                verbose = true;
            }
            "--delay" => {
                let val = args.next().unwrap_or_else(|| {
                    eprintln!("--delay requires a value");
                    std::process::exit(1);
                });
                initial_delay_secs = val.parse().unwrap_or_else(|_| {
                    eprintln!("--delay: expected a number, got {val}");
                    std::process::exit(1);
                });
                // Duration::from_secs_f64 panics on negative / NaN / inf.
                if !initial_delay_secs.is_finite() || initial_delay_secs < 0.0 {
                    eprintln!("--delay: expected a non-negative number, got {val}");
                    std::process::exit(1);
                }
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
            name => match parse_step(name) {
                Ok(step) => steps.push(step),
                Err(e) => {
                    eprintln!("error: {e}");
                    eprintln!("Run with --help for the list of available actions.");
                    std::process::exit(1);
                }
            },
        }
    }

    if steps.is_empty() {
        eprintln!("error: no actions specified");
        print_usage();
        std::process::exit(1);
    }

    let default_level = if verbose { "debug" } else { "warn" };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

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
        steps.len(),
        initial_delay_secs
    );
    std::thread::sleep(delay);

    let between = Duration::from_millis(between_ms);
    for (i, step) in steps.iter().enumerate() {
        fire(step);
        if i + 1 < steps.len() {
            std::thread::sleep(between);
        }
    }
    println!("Done.");
}

fn fire(step: &Step) {
    match step {
        Step::Action(action) => {
            println!("  → {}", action.label());
            openlogi_inject::execute(action);
        }
        Step::PanBegin => {
            println!("  → PanBegin");
            openlogi_inject::post_pan_begin();
        }
        Step::Pan { dx, dy } => {
            println!("  → Pan({dx}, {dy})");
            openlogi_inject::post_pan(*dx, *dy);
        }
        Step::PanEnd => {
            println!("  → PanEnd");
            openlogi_inject::post_pan_end();
        }
        Step::Zoom(amount) => {
            println!("  → Zoom({amount})");
            openlogi_inject::post_zoom_continuous(*amount);
        }
        Step::ZoomEnd => {
            println!("  → ZoomEnd");
            openlogi_inject::post_zoom_end();
        }
        Step::FlushGestures => {
            println!("  → FlushGestures");
            openlogi_inject::flush_gesture_sessions();
        }
    }
}

fn print_usage() {
    eprintln!(
        "Usage: inject_action [--delay <secs>] [--between <ms>] [-v] <Action> [<Action> ...]\n\
         \n\
         Options:\n\
           --delay <secs>    seconds to wait before first injection (default: 2)\n\
           --between <ms>    milliseconds between actions (default: 200)\n\
           -v, --verbose     enable debug logging (shows which code path fires)\n\
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
                  PanBegin Pan:<dx>:<dy> PanEnd Zoom:<amount> ZoomEnd FlushGestures\n\
         \n\
         CustomShortcut modifier bits: 0x01=Cmd/Ctrl 0x02=Shift 0x04=Ctrl 0x08=Option/Alt\n\
         CustomShortcut key_hex: macOS kVK_* code (e.g. 0x08=C, 0x09=V, 0x7E=Up)\n\
         Pan:<dx>:<dy> is screen pixels (+x right, +y down). Zoom:<amount> zooms in when positive.\n\
         \n\
         Examples:\n\
           inject_action --delay 3 Copy\n\
           inject_action --delay 2 --between 500 VolumeUp VolumeDown PlayPause\n\
           inject_action ScrollDown ScrollDown ScrollDown\n\
           inject_action CustomShortcut:0x01:0x08   # Ctrl+C\n\
           inject_action --delay 2 PanBegin Pan:8:6 Pan:8:6 PanEnd\n\
           inject_action --delay 2 Zoom:0.05 Zoom:0.05 ZoomEnd"
    );
}
