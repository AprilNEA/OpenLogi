//! Smoke-test for `pointer_context()`: prints the target under the pointer
//! once per second.
//!
//! ```text
//! cargo run --example pointer_context -p openlogi-hook
//! ```

fn main() {
    println!("Polling the window under the pointer every second — move it to test.");
    std::thread::spawn(|| {
        loop {
            let context = openlogi_hook::pointer_context();
            match context.app {
                Some(app) => println!("{:?}\t{}", context.target, app.id),
                None => println!("{:?}", context.target),
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
    // The macOS lookup runs on the AppKit main loop, as in the agent.
    #[cfg(target_os = "macos")]
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        objc2_app_kit::NSApplication::sharedApplication(mtm).run();
    }
    loop {
        std::thread::park();
    }
}
