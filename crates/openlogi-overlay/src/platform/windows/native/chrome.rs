//! Strip the square DWM and USER32 fill behind the transparent ring HWND, so
//! only what GPUI paints is visible.

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DWMSBT_NONE, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_COLOR_NONE,
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
    DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{CreateEllipticRgn, DeleteObject, SetWindowRgn};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CS_DROPSHADOW, GCL_STYLE, GCLP_HBRBACKGROUND, GetClassLongPtrW, GetWindowRect, SetClassLongPtrW,
};

/// Stop DWM, the class brush, and the accent policy from painting a square
/// behind the transparent ring. The ellipse region drops the square corners;
/// the pill slots sit inside that circle.
///
/// Call after the final size: the ellipse region is the window rect, and a
/// region taken from the 1px bootstrap would stay clipped to a point.
#[expect(
    unsafe_code,
    reason = "DWM, class style, accent policy, and region calls on the borrowed ring HWND"
)]
pub(super) fn clear_square_fill(hwnd: HWND) {
    set_dwm_color(hwnd, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE);
    set_dwm_color(hwnd, DWMWA_CAPTION_COLOR, DWMWA_COLOR_NONE);
    set_dwm_color(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE,
        DWMWCP_DONOTROUND.cast_unsigned(),
    );
    set_dwm_color(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_NONE.cast_unsigned());
    // SAFETY: hwnd belongs to this process. GCL_STYLE and GCLP_HBRBACKGROUND
    // are the documented class indexes. CS_DROPSHADOW is the square HWND
    // shadow; a null background brush stops USER32 filling the client before
    // DirectComposition paints. The overlay's other windows share the class,
    // so this also clears their shadow and brush.
    unsafe {
        let style = GetClassLongPtrW(hwnd, GCL_STYLE);
        let cleared = style & !(CS_DROPSHADOW as usize);
        let _ = SetClassLongPtrW(hwnd, GCL_STYLE, cleared.cast_signed());
        let _ = SetClassLongPtrW(hwnd, GCLP_HBRBACKGROUND, 0);
    }
    disable_composition_fill(hwnd);
    clip_hwnd_to_ellipse(hwnd);
}

/// `DwmSetWindowAttribute` for a 4-byte value. `hwnd` is the live ring window.
#[expect(
    unsafe_code,
    reason = "DwmSetWindowAttribute on the borrowed ring HWND"
)]
fn set_dwm_color(hwnd: HWND, attribute: i32, value: u32) {
    // SAFETY: hwnd is the ring window on its UI thread, and `value` lives for the call.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            attribute.cast_unsigned(),
            std::ptr::from_ref(&value).cast(),
            4,
        );
    }
}

#[repr(C)]
struct AccentPolicy {
    accent_state: u32,
    accent_flags: u32,
    gradient_color: u32,
    animation_id: u32,
}

#[repr(C)]
struct WindowCompositionAttribData {
    attrib: u32,
    data: *mut core::ffi::c_void,
    size: usize,
}

#[expect(
    unsafe_code,
    reason = "SetWindowCompositionAttribute is undocumented and loaded by name"
)]
fn disable_composition_fill(hwnd: HWND) {
    type SetWindowCompositionAttribute =
        unsafe extern "system" fn(HWND, *mut WindowCompositionAttribData) -> i32;
    /// WCA_ACCENT_POLICY. GPUI's transparent appearance sets accent state 2,
    /// which paints a solid square plus a drop shadow.
    const WCA_ACCENT_POLICY: u32 = 0x13;

    // SAFETY: the module name is a process-lifetime C string. A null module
    // means the lookup failed and the accent override is skipped.
    let user32 = unsafe { GetModuleHandleA(c"user32.dll".as_ptr().cast()) };
    if user32.is_null() {
        return;
    }
    // SAFETY: user32 is a live module; the proc name is a process-lifetime C string.
    let Some(raw) =
        (unsafe { GetProcAddress(user32, c"SetWindowCompositionAttribute".as_ptr().cast()) })
    else {
        return;
    };
    // SAFETY: GetProcAddress returned the symbol of this name. The Win32
    // contract is that it matches SetWindowCompositionAttribute's ABI.
    let set: SetWindowCompositionAttribute = unsafe { std::mem::transmute(raw) };
    // accent_state 0 is ACCENT_DISABLED. DirectComposition already owns the
    // swapchain; any non-zero accent fills the HWND with a grey card.
    let mut accent = AccentPolicy {
        accent_state: 0,
        accent_flags: 0,
        gradient_color: 0,
        animation_id: 0,
    };
    let mut data = WindowCompositionAttribData {
        attrib: WCA_ACCENT_POLICY,
        data: std::ptr::from_mut(&mut accent).cast(),
        size: size_of::<AccentPolicy>(),
    };
    // SAFETY: data points at accent for the duration of the call.
    unsafe {
        let _ = set(hwnd, std::ptr::from_mut(&mut data));
    }
}

#[expect(
    unsafe_code,
    reason = "GetWindowRect and SetWindowRgn on the borrowed ring HWND"
)]
fn clip_hwnd_to_ellipse(hwnd: HWND) {
    let mut rect = RECT::default();
    // SAFETY: hwnd is a live top-level window of this process; rect is writable
    // for the duration of the call.
    if unsafe { GetWindowRect(hwnd, &raw mut rect) } == 0 {
        return;
    }
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width <= 0 || height <= 0 {
        return;
    }
    // SAFETY: the ellipse is in window coordinates. SetWindowRgn takes ownership
    // of the region when it succeeds; on failure the caller still owns it.
    unsafe {
        let region = CreateEllipticRgn(0, 0, width, height);
        if region.is_null() {
            return;
        }
        if SetWindowRgn(hwnd, region, 1) == 0 {
            let _ = DeleteObject(region);
        }
    }
}
