use super::Motion;

#[must_use]
pub(super) fn read() -> Option<Motion> {
    let reduced =
        objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion();
    Some(if reduced {
        Motion::Reduced
    } else {
        Motion::Full
    })
}
