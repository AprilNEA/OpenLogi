use super::Motion;

#[must_use]
pub(super) fn read() -> Option<Motion> {
    client_area_animation_enabled().map(|enabled| {
        if enabled {
            Motion::Full
        } else {
            Motion::Reduced
        }
    })
}

#[expect(
    unsafe_code,
    reason = "SystemParametersInfoW is plain Win32 FFI; std and windows-sys offer no safe reader for SPI_GETCLIENTAREAANIMATION"
)]
fn client_area_animation_enabled() -> Option<bool> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
    };

    let mut enabled = 0i32;
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes a single BOOL to pvParam; `enabled` is a
    // live, properly aligned i32 for the duration of the call, and uiParam/fWinIni are unused.
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut enabled).cast(), 0)
    };
    if ok == 0 {
        return None;
    }
    Some(enabled != 0)
}
