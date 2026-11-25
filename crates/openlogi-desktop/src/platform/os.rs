//! Best-effort host OS version string for the diagnostics report, plus syncing
//! the native window chrome (titlebar) to the in-app appearance preference,
//! and reading raw event details that GPUI does not expose.

use openlogi_core::config::Appearance;

#[cfg(target_os = "macos")]
fn current_modifier_event() -> Option<objc2::rc::Retained<objc2_app_kit::NSEvent>> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEventType};

    NSApplication::sharedApplication(MainThreadMarker::new()?)
        .currentEvent()
        .filter(|event| event.r#type() == NSEventType::FlagsChanged)
}

/// Whether AppKit is dispatching the right Control key's modifier event.
pub(crate) fn current_event_is_right_control() -> bool {
    #[cfg(target_os = "macos")]
    {
        current_modifier_event().is_some_and(|event| event.keyCode() == 0x3e)
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Whether the right Control key is currently down during a modifier event.
pub(crate) fn current_event_has_right_control() -> bool {
    #[cfg(target_os = "macos")]
    {
        // NX_DEVICERCTLKEYMASK from IOLLEvent.h, below the independent flags.
        current_modifier_event().is_some_and(|event| event.modifierFlags().bits() & 0x2000 != 0)
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// The OS product version (e.g. `"15.5"` on macOS), or `None` when unavailable.
#[must_use]
// Only the macOS arm returns `Some`, so the lint fires there and nowhere else.
#[cfg_attr(
    target_os = "macos",
    expect(
        clippy::unnecessary_wraps,
        reason = "Option is the cross-platform contract; non-macOS arms return None"
    )
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
        // SAFETY: `NSAppearanceNameAqua` is an AppKit `extern static` typed
        // `&'static NSAppearanceName`; its non-lazy binding is resolved when the
        // framework loads, before `main`, and AppKit never reassigns it — so
        // this is a shared read of a live `&'static` of the declared type.
        Appearance::Light => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua }),
        // SAFETY: `NSAppearanceNameDarkAqua` is an AppKit `extern static` typed
        // `&'static NSAppearanceName`; its non-lazy binding is resolved when the
        // framework loads, before `main`, and AppKit never reassigns it — so
        // this is a shared read of a live `&'static` of the declared type.
        Appearance::Dark => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }),
    };
    NSApplication::sharedApplication(mtm).setAppearance(named.as_deref());
}

#[cfg(not(target_os = "macos"))]
pub fn set_app_appearance(_appearance: Appearance) {}
