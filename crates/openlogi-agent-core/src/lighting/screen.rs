//! Primary-display average colour for the screen-sampler host effect.

/// Whether this build compiled a screen-sampler backend.
#[must_use]
pub fn available() -> bool {
    cfg!(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "windows"
    ))
}

/// Average colour of a centre-crop of the primary display.
#[must_use]
pub fn sample_primary() -> Option<(u8, u8, u8)> {
    sample_impl()
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn sample_impl() -> Option<(u8, u8, u8)> {
    request_permission();
    let monitors = xcap::Monitor::all().ok()?;
    let monitor = monitors
        .iter()
        .find(|monitor| is_primary(monitor))
        .or_else(|| monitors.first())?;
    let image = monitor.capture_image().ok()?;
    average_center(&image)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn is_primary(monitor: &xcap::Monitor) -> bool {
    monitor.is_primary().unwrap_or(false)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn sample_impl() -> Option<(u8, u8, u8)> {
    None
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn average_center(image: &image::RgbaImage) -> Option<(u8, u8, u8)> {
    let width = image.width();
    let height = image.height();
    if width < 4 || height < 4 {
        return None;
    }
    let x0 = width / 4;
    let y0 = height / 4;
    let x1 = width.saturating_sub(width / 4);
    let y1 = height.saturating_sub(height / 4);
    let mut r = 0u64;
    let mut g = 0u64;
    let mut b = 0u64;
    let mut count = 0u64;
    for y in (y0..y1).step_by(4) {
        for x in (x0..x1).step_by(4) {
            let pixel = image.get_pixel(x, y).0;
            r += u64::from(pixel[0]);
            g += u64::from(pixel[1]);
            b += u64::from(pixel[2]);
            count += 1;
        }
    }
    if count == 0 {
        return None;
    }
    Some((
        u8::try_from(r / count).unwrap_or(0),
        u8::try_from(g / count).unwrap_or(0),
        u8::try_from(b / count).unwrap_or(0),
    ))
}

#[cfg(target_os = "macos")]
fn request_permission() {
    use std::sync::Once;
    static ASKED: Once = Once::new();
    ASKED.call_once(|| {
        request_screen_capture_access();
    });
}

/// Prompt for Screen Recording from the agent (the process that captures).
#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "CoreGraphics Screen Capture TCC probes have no safe wrapper in this crate"
)]
fn request_screen_capture_access() {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }
    // SAFETY: documented Screen Capture TCC probes; Request prompts only
    // when Preflight is false, and only this agent process should prompt.
    if !unsafe { CGPreflightScreenCaptureAccess() } {
        let _ = unsafe { CGRequestScreenCaptureAccess() };
    }
}

#[cfg(not(target_os = "macos"))]
fn request_permission() {}
