//! Best-effort host OS version string for the diagnostics report, plus syncing
//! the native window chrome (titlebar) to the in-app appearance preference.

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
