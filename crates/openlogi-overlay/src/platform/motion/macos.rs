//! The macOS reduce-motion preference: `NSWorkspace`'s accessibility
//! setting, live on every read.

use objc2_app_kit::NSWorkspace;

use super::MotionSetting;

/// Always a live value: AppKit answers without a call that can fail.
#[expect(
    clippy::unnecessary_wraps,
    reason = "the facade's reader is fallible on the other platforms"
)]
pub(super) fn read() -> Option<MotionSetting> {
    let reduced = NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion();
    Some(if reduced {
        MotionSetting::Reduced
    } else {
        MotionSetting::Full
    })
}
