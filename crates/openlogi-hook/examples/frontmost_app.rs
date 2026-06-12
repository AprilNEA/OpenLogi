//! Smoke-test for `frontmost_bundle_id()` on Linux.
//!
//! Polls the focused application once per second and prints its identifier.
//! Switch between windows while it runs to verify detection.
//!
//! # Usage
//!
//! ```text
//! cargo build --example frontmost_app -p openlogi-hook
//! ./target/debug/examples/frontmost_app
//! ```

#[cfg(target_os = "linux")]
fn main() {
    println!("Polling focused app every second — switch windows to test.");
    loop {
        println!("{:?}", openlogi_hook::frontmost_bundle_id());
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("frontmost_app is a Linux-only smoke test (no-op on this platform).");
}
