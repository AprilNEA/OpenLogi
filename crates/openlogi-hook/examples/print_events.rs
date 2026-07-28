//! Manual smoke-test for the OS-level mouse hook.
//!
//! Prints every mouse event to stdout and passes all events through unchanged.
//! Press Ctrl-C to stop.
//!
//! # Linux permissions
//!
//! Requires read access to `/dev/input/eventN` and write access to
//! `/dev/uinput`. Add your user to the `input` group and apply a udev rule:
//!
//! ```sh
//! sudo usermod -aG input $USER
//! echo 'KERNEL=="uinput", GROUP="input", MODE="0660"' \
//!     | sudo tee /etc/udev/rules.d/99-uinput.rules
//! sudo udevadm trigger /dev/uinput
//! # log out and back in, then:
//! cargo run --example print_events -p openlogi-hook
//! ```

// Linux-only smoke test. A crate-level `#![cfg(target_os = "linux")]` would
// leave an empty crate with no `main` on other targets (E0601), breaking
// `cargo build --all-targets` there — so gate the body on `main` instead and
// provide a trivial fallback.
#[cfg(target_os = "linux")]
fn main() {
    use openlogi_hook::{EventDisposition, Hook};

    let hook = match Hook::start(|event| {
        println!("{event:?}");
        EventDisposition::PassThrough
    }) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: failed to start hook: {e}");
            std::process::exit(1);
        }
    };

    println!("Hook running — move the mouse or click buttons. Press Ctrl-C to stop.");

    // Block until Ctrl-C.
    let (tx, rx) = std::sync::mpsc::channel();
    #[expect(
        clippy::expect_used,
        reason = "example binary; aborting on a failed handler install is fine"
    )]
    ctrlc::set_handler(move || {
        let _ = tx.send(());
    })
    .expect("failed to set Ctrl-C handler");
    rx.recv().ok();

    hook.stop();
    println!("Hook stopped.");
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("print_events is a Linux-only smoke test (no-op on this platform).");
}
