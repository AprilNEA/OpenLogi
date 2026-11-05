use super::Motion;

#[must_use]
#[expect(
    clippy::unnecessary_wraps,
    reason = "macOS always returns a live preference; the shared signature uses None for read failure on other platforms"
)]
pub(super) fn read() -> Option<Motion> {
    let reduced =
        objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion();
    Some(if reduced {
        Motion::Reduced
    } else {
        Motion::Full
    })
}
