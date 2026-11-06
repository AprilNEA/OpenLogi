//! A semantic AppKit material owned by each window's public view hierarchy.

use gpui::{Window, WindowBackgroundAppearance};
use objc2::MainThreadOnly as _;
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSUserInterfaceItemIdentification as _, NSView,
    NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    NSWindow, NSWindowOrderingMode, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{MainThreadMarker, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

const MATERIAL_IDENTIFIER: &str = "org.openlogi.openlogi.window-material";

/// Install or remove only the view that this adapter owns.
#[expect(
    unsafe_code,
    reason = "borrow the live AppKit NSView from GPUI's window handle"
)]
pub(super) fn apply(window: &Window, material: WindowBackgroundAppearance) {
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("window material requires the main thread");
        return;
    };
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    // SAFETY: GPUI provides its live render NSView through this borrowed AppKit
    // handle. The Window and handle stay alive for this call. MainThreadMarker
    // proves that all view access occurs on the AppKit main thread.
    let render_view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    let Some(native_window) = render_view.window() else {
        return;
    };
    apply_titlebar(&native_window, material);
    let Some(content) = native_window.contentView() else {
        return;
    };
    let identifier = NSString::from_str(MATERIAL_IDENTIFIER);
    let subviews = content.subviews();
    for child in &subviews {
        if child.identifier().as_deref() == Some(identifier.as_ref()) {
            if material == WindowBackgroundAppearance::Transparent {
                child.setFrame(content.bounds());
            } else {
                child.removeFromSuperview();
            }
            return;
        }
    }
    if material != WindowBackgroundAppearance::Transparent {
        return;
    }

    let effect =
        NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), content.bounds());
    effect.setIdentifier(Some(&identifier));
    // UnderWindowBackground is available since macOS 10.14, below our 13.0
    // floor. AppKit retains its native tint, accessibility behavior, and blur.
    effect.setMaterial(NSVisualEffectMaterial::UnderWindowBackground);
    effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    effect.setState(NSVisualEffectState::Active);
    effect.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    // The parent retains the material until removal or window destruction.
    // GPUI's render view remains above it and owns all input handling.
    content.addSubview_positioned_relativeTo(
        &effect,
        NSWindowOrderingMode::Below,
        Some(render_view),
    );
}

/// Change only the content-layout bit. Keep fullscreen and all other flags.
fn titlebar_style_mask(
    mut current: NSWindowStyleMask,
    material: WindowBackgroundAppearance,
) -> NSWindowStyleMask {
    current.set(
        NSWindowStyleMask::FullSizeContentView,
        material == WindowBackgroundAppearance::Transparent,
    );
    current
}

fn apply_titlebar(window: &NSWindow, material: WindowBackgroundAppearance) {
    let transparent = material == WindowBackgroundAppearance::Transparent;
    let current = window.styleMask();
    let updated = titlebar_style_mask(current, material);
    // Bounds notifications reapply this policy. Avoid another layout pass when
    // the current style already matches, including during fullscreen changes.
    if updated != current {
        window.setStyleMask(updated);
    }
    if window.titlebarAppearsTransparent() != transparent {
        window.setTitlebarAppearsTransparent(transparent);
    }
    let visibility = if transparent {
        NSWindowTitleVisibility::Hidden
    } else {
        NSWindowTitleVisibility::Visible
    };
    if window.titleVisibility() != visibility {
        window.setTitleVisibility(visibility);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titlebar_transitions_preserve_fullscreen_and_other_window_flags() {
        let normal = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        for original in [
            normal,
            normal | NSWindowStyleMask::FullScreen,
            normal | NSWindowStyleMask::UtilityWindow,
            normal | NSWindowStyleMask::from_bits_retain(1 << 29),
        ] {
            let glass = titlebar_style_mask(original, WindowBackgroundAppearance::Transparent);
            assert!(glass.contains(NSWindowStyleMask::FullSizeContentView));
            assert_eq!(
                glass.bits() & !NSWindowStyleMask::FullSizeContentView.bits(),
                original.bits(),
            );
            assert_eq!(
                titlebar_style_mask(glass, WindowBackgroundAppearance::Transparent),
                glass,
            );
            assert_eq!(
                titlebar_style_mask(glass, WindowBackgroundAppearance::Opaque),
                original,
            );
        }
    }
}
