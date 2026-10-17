//! Smoke-test for `frontmost_application()`.
//!
//! Polls the focused application once per second and prints its identifier and
//! display name — the two halves per-app profiles are keyed and labelled by.
//! Switch between windows while it runs to verify detection.
//!
//! Worth pointing at every platform that has a frontmost reader: the
//! identifier's shape differs on each (bundle id, `WM_CLASS`, xdg `app_id`,
//! executable path). On a Wayland session it is also how you find out whether
//! the session resolved to a usable backend at all — a `None` here is the
//! reason per-app profiles would silently never switch there.
//!
//! # Usage
//!
//! ```text
//! cargo run --example frontmost_app -p openlogi-hook
//! ```

fn main() {
    println!("Polling the focused app every second — switch windows to test.");
    loop {
        match openlogi_hook::frontmost_application() {
            Some(app) => println!("{}\t{}", app.id, app.display_name),
            None => println!("(none — no frontmost window, or no reader on this platform)"),
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
