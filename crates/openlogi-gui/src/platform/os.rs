//! Best-effort host OS version string for the diagnostics report, plus the
//! native window chrome: syncing the titlebar to the in-app appearance
//! preference, and pointing the main window's backdrop at a real macOS
//! material.

use openlogi_core::config::Appearance;

/// The OS product version (e.g. `"15.5"` on macOS), or `None` when unavailable.
#[must_use]
#[allow(
    clippy::unnecessary_wraps,
    reason = "Option is the cross-platform contract; non-macOS arms return None"
)]
pub fn os_version() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let v = objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion();
        Some(if v.patchVersion == 0 {
            format!("{}.{}", v.majorVersion, v.minorVersion)
        } else {
            format!("{}.{}.{}", v.majorVersion, v.minorVersion, v.patchVersion)
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Sync the **whole app's** native chrome (system titlebar, traffic lights) to
/// the appearance preference, so a forced light/dark theme isn't betrayed by a
/// titlebar that still tracks the OS. `System` clears the override (the chrome
/// follows the OS, matching the resolved theme); `Light` / `Dark` pin the
/// matching `NSAppearance`. No-op off macOS, where window chrome isn't ours to
/// paint this way.
#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "reading the framework's NSAppearanceName statics to set NSApp.appearance"
)]
pub fn set_app_appearance(appearance: Appearance) {
    use objc2_app_kit::{
        NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication,
    };
    use objc2_foundation::MainThreadMarker;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let named = match appearance {
        Appearance::System => None,
        // SAFETY: the `NSAppearanceName` constants are static framework globals,
        // valid for the whole process; `appearanceNamed` copies what it needs.
        Appearance::Light => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua }),
        Appearance::Dark => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }),
    };
    NSApplication::sharedApplication(mtm).setAppearance(named.as_deref());
}

#[cfg(not(target_os = "macos"))]
pub fn set_app_appearance(_appearance: Appearance) {}

/// Retarget the main window's native backdrop at the macOS window-background
/// material.
///
/// GPUI already installs an `NSVisualEffectView` beneath its Metal layer for a
/// window that asks for [`WindowBackgroundAppearance::Blurred`][blurred], but
/// it picks `NSVisualEffectMaterial::Selection` — a highlight tint, not a
/// window backdrop. We retarget the view GPUI owns rather than stacking one of
/// our own, which would composite two vibrancies and read as muddy.
///
/// Note what this does *not* buy: GPUI's `BlurredView` overrides `updateLayer`
/// to nil the material's background colour, hide its `CAChameleonLayer`
/// (desktop tinting) and strip its saturation filter, so no material here
/// renders AppKit's full look. What survives is the blur, and the material
/// chooses its radius — `WindowBackground`'s is the one sized for a window
/// backdrop. The colour comes from the GPUI side instead, as a single
/// translucent fill: [`Palette::backdrop`](crate::theme::Palette::backdrop).
///
/// Call once per window, after it opens. The view persists for the window's
/// life and carries no colour of its own, so a light/dark flip needs no
/// re-apply.
///
/// [blurred]: gpui::WindowBackgroundAppearance::Blurred
#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "reaching GPUI's NSView through its raw window handle; GPUI exposes no material API"
)]
pub fn configure_window_material(window: &gpui::Window) {
    use objc2_app_kit::{NSView, NSVisualEffectMaterial, NSVisualEffectView};
    use objc2_foundation::MainThreadMarker;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // Spelled out: gpui's inherent `Window::window_handle` returns its own
    // `AnyWindowHandle` and would shadow the trait method here.
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    // AppKit access stays on the main thread; possessing the marker proves it.
    let Some(_mtm) = MainThreadMarker::new() else {
        return;
    };

    // SAFETY: `ns_view` is the live `NSView` GPUI created for this window and
    // owns for its lifetime, so the borrow cannot outlive the view. We only
    // read the view hierarchy and set a public AppKit property on a subview
    // GPUI does not otherwise configure after creation.
    unsafe {
        let view = handle.ns_view.cast::<NSView>().as_ref();
        let Some(content_view) = view.window().and_then(|window| window.contentView()) else {
            return;
        };
        for subview in content_view.subviews() {
            if let Some(effect_view) = subview.downcast_ref::<NSVisualEffectView>() {
                effect_view.setMaterial(NSVisualEffectMaterial::WindowBackground);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn configure_window_material(_window: &gpui::Window) {}

/// Tell gpui whether the user asked the system to reduce motion.
///
/// gpui's `with_animation` already honours `App::reduce_motion`, but nothing
/// ever *set* it, so the flag sat at its default and every animation ran
/// regardless of the system preference. Call once at startup, before the
/// window opens.
///
/// A direct `window.request_animation_frame` for decorative motion is not
/// covered by that flag and must check `cx.reduce_motion()` itself.
#[cfg(target_os = "macos")]
pub fn init_reduce_motion(cx: &mut gpui::App) {
    use objc2_app_kit::NSWorkspace;

    cx.set_reduce_motion(NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion());
}

/// GNOME exposes the same preference through GSettings.
///
/// Read synchronously, like the macOS arm. It shells out, which is exactly the
/// kind of work the render path must never touch — but this runs once before
/// the first window exists, and resolving it in the background would let that
/// window (and its first animation) be created while the flag still held its
/// default, quietly ignoring the preference for the moment that matters most.
#[cfg(target_os = "linux")]
pub fn init_reduce_motion(cx: &mut gpui::App) {
    cx.set_reduce_motion(gnome_animations_disabled());
}

#[cfg(target_os = "linux")]
fn gnome_animations_disabled() -> bool {
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "enable-animations"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("false"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn init_reduce_motion(_cx: &mut gpui::App) {}
