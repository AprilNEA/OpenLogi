//! Manual smoke-test for software scroll inversion (#694, #776).
//!
//! Grabs the Logitech pointers the hook can see and runs two windows of equal
//! length — the first passing the wheel through untouched, the second
//! inverting it — then releases everything, so a wedged run cannot leave the
//! mouse captured. Scroll in both: the direction must flip at the marker, and
//! horizontal (thumb-wheel) scrolling must be untouched in either.
//!
//! Two phases rather than one because a single window cannot tell an applied
//! inversion from a desktop that was already scrolling that way.
//!
//! ```sh
//! cargo run --example invert_scroll -p openlogi-hook -- 20
//! ```
//!
//! Linux permissions are the same as `print_events`: read access to
//! `/dev/input/eventN` and write access to `/dev/uinput`.

// Gated on `main` rather than the crate, so `cargo build --all-targets` still
// finds a `main` on the platforms that cannot run this (E0601).
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn main() {
    use std::time::Duration;

    use openlogi_hook::{EventDisposition, Hook, HookEvent, MouseEvent};

    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(20);

    if !openlogi_hook::SOFTWARE_SCROLL_INVERSION {
        println!("software scroll inversion is not available on this platform");
        return;
    }

    let hook = match Hook::start(|event| {
        if let HookEvent::Mouse(MouseEvent::Scroll {
            delta,
            from_trackpad,
            device,
        }) = &event
        {
            println!(
                "scroll {delta:?} trackpad={from_trackpad} device={:?}",
                device.as_ref().and_then(|d| d.product_name.as_deref())
            );
        }
        EventDisposition::PassThrough
    }) {
        Ok(hook) => hook,
        Err(e) => {
            eprintln!("could not start the hook: {e}");
            return;
        }
    };

    // Two phases, so the answer does not depend on knowing what the desktop's
    // own scroll direction was to begin with: the same wheel is scrolled in
    // both, and only the second one is transformed. If the direction does not
    // change at the marker, the transform is not reaching the desktop.
    openlogi_hook::set_scroll_inversion(false);
    println!("=== PHASE A ({seconds}s): inversion OFF — scroll and note the direction");
    std::thread::sleep(Duration::from_secs(seconds));

    openlogi_hook::set_scroll_inversion(true);
    println!("=== PHASE B ({seconds}s): inversion ON — scroll again; it must be the opposite");
    std::thread::sleep(Duration::from_secs(seconds));

    openlogi_hook::set_scroll_inversion(false);
    println!("=== done — releasing the grab");
    hook.stop();
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn main() {
    println!("software scroll inversion is not available on this platform");
}
