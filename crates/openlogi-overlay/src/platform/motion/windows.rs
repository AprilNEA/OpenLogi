//! The Windows reduce-motion preference: whether client-area animation
//! ("Animation effects" in Settings) is on.

use windows_sys::Win32::UI::WindowsAndMessaging::{
    SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
};

use super::MotionSetting;

/// `None` when Win32 refuses the query.
#[expect(
    unsafe_code,
    reason = "SystemParametersInfoW is plain Win32 FFI; std and windows-sys offer no safe reader for SPI_GETCLIENTAREAANIMATION"
)]
pub(super) fn read() -> Option<MotionSetting> {
    let mut enabled = 0i32;
    // SAFETY: `SPI_GETCLIENTAREAANIMATION` writes one `BOOL` when the call succeeds.
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut enabled).cast(), 0)
    };
    if ok == 0 {
        return None;
    }
    Some(MotionSetting::from_animations(enabled != 0))
}
