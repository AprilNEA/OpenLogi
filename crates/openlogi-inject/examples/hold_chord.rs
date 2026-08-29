//! Manual smoke-test for `openlogi_inject::press_hold`.
//!
//! Holds one chord for a fixed time, then releases it, so the two hold shapes
//! can be watched side by side against a real keyboard.
//!
//! # Usage
//!
//! ```text
//! cargo run --example hold_chord -p openlogi-inject -- \
//!     [--delay <secs>] [--hold <ms>] [--whole-chord] <chord>
//! ```
//!
//! `--whole-chord` selects `HeldKeys::WholeChord`, what `Action::HoldShortcut`
//! does. The default is `HeldKeys::ModifiersOnly`, what
//! `Action::TapKeyHoldingModifiers` does.
//!
//! ```text
//! # Cmd stays down for 3 s and Tab is tapped once: the application switcher
//! # steps one window forward and stays open until the release.
//! cargo run --example hold_chord -p openlogi-inject -- --hold 3000 Cmd+Tab
//!
//! # The same chord held whole: key repeat walks the switcher to its last
//! # window and parks there.
//! cargo run --example hold_chord -p openlogi-inject -- --hold 3000 --whole-chord Cmd+Tab
//! ```
//!
//! To watch the edges rather than the effect, point an event monitor at the
//! session tap and compare the two shapes: the ordinary key up edge follows its
//! down edge by milliseconds under `ModifiersOnly` and by the whole hold under
//! `WholeChord`. A chord no system hotkey claims, such as `Shift+F13`, keeps
//! the reading about the injection alone.

use std::process::ExitCode;
use std::thread::sleep;
use std::time::Duration;

use openlogi_core::binding::{HeldKeys, KeyCombo};

const USAGE: &str = "usage: hold_chord [--delay <secs>] [--hold <ms>] [--whole-chord] <chord>";

struct Options {
    delay: Duration,
    hold: Duration,
    keys: HeldKeys,
    chord: KeyCombo,
}

fn main() -> ExitCode {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("hold_chord: {message}\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    println!(
        "holding {} as {:?} in {:.1} s for {} ms",
        options.chord.rendered_label(),
        options.keys,
        options.delay.as_secs_f64(),
        options.hold.as_millis()
    );
    sleep(options.delay);

    let chord = openlogi_inject::press_hold(&options.chord, options.keys);
    sleep(options.hold);
    drop(chord);

    println!("released");
    ExitCode::SUCCESS
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut delay = Duration::from_secs(2);
    let mut hold = Duration::from_millis(3000);
    let mut keys = HeldKeys::ModifiersOnly;
    let mut chord = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--delay" => delay = seconds(&value(&mut args, "--delay")?)?,
            "--hold" => hold = milliseconds(&value(&mut args, "--hold")?)?,
            "--whole-chord" => keys = HeldKeys::WholeChord,
            other => {
                chord = Some(
                    other
                        .parse::<KeyCombo>()
                        .map_err(|error| format!("{other}: {error}"))?,
                );
            }
        }
    }

    Ok(Options {
        delay,
        hold,
        keys,
        chord: chord.ok_or("a chord is required")?,
    })
}

fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn seconds(raw: &str) -> Result<Duration, String> {
    // `Duration::from_secs_f64` panics on a negative, NaN, or infinite input.
    let secs: f64 = raw
        .parse()
        .map_err(|_| format!("--delay: expected a number, got {raw}"))?;
    if !secs.is_finite() || secs < 0.0 {
        return Err(format!(
            "--delay: expected a non-negative number, got {raw}"
        ));
    }
    Ok(Duration::from_secs_f64(secs))
}

fn milliseconds(raw: &str) -> Result<Duration, String> {
    raw.parse()
        .map(Duration::from_millis)
        .map_err(|_| format!("--hold: expected a number of milliseconds, got {raw}"))
}
