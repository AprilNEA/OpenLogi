//! Thin `objc2` wrappers over the macOS `NSStatusItem` / `NSMenu` primitives,
//! used by [`crate::tray`] to host the menu-bar item from the headless agent.
//!
//! Ownership is a value: every object is a [`Retained<T>`] that releases on
//! `Drop`, so the issue-#99 `CFString` leak (the old raw-`id` path) can't be
//! written. The only `unsafe` calls — `initWithTitle:action:keyEquivalent:` and
//! `setTarget:` (raw selector + a *weak* target reference) — are wrapped here.

#![expect(
    unsafe_code,
    reason = "the two Objective-C calls objc2 marks unsafe (init-with-action, set-target) are wrapped here"
)]

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem};
use objc2_foundation::{NSData, NSString};

/// `NSVariableStatusItemLength` — a status item sized to its content.
const VARIABLE_LENGTH: f64 = -1.0;

/// Create and return a variable-width status item. The returned [`Retained`]
/// owns it; the tray keeps it for the app's lifetime.
pub(crate) fn create_status_item() -> Retained<NSStatusItem> {
    NSStatusBar::systemStatusBar().statusItemWithLength(VARIABLE_LENGTH)
}

/// Create a menu with AppKit auto-enabling disabled (the agent manages item
/// state itself).
pub(crate) fn new_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    menu
}

/// Create an action item that sends `action` to `target` when clicked.
///
/// `target` is stored as a *weak* reference by AppKit, so the caller must keep
/// it alive for as long as the item can be clicked (the tray holds the
/// `Retained` target for the app's lifetime).
///
/// `key` is the key-equivalent string (e.g. `"m"` for ⌘M); pass `""` for none.
/// The default modifier mask is ⌘ (Command).
pub(crate) fn new_action_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Sel,
    target: &AnyObject,
    key: &str,
) -> Retained<NSMenuItem> {
    // SAFETY: `initWithTitle:action:keyEquivalent:` is NSMenuItem's designated
    // initializer; the two `NSString`s outlive the call and `action` is a
    // selector `target` responds to (wired up by `setTarget:` below).
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        )
    };
    // SAFETY: `target` is a live Objective-C object that responds to `action`.
    // NSMenuItem keeps only a weak reference, so the caller retains `target`
    // (see the doc comment) — there is no dangling-target window.
    unsafe { item.setTarget(Some(target)) };
    item
}

/// The point height of a menu-bar status item's icon (AppKit convention; the
/// status bar is ~22pt tall, leaving ~18pt for the icon).
const STATUS_ICON_POINTS: f64 = 18.0;

/// Set a custom PNG as the status-item icon (template image). Pass the @2x PNG
/// data; the image's logical size is pinned to the fixed menu-bar point size,
/// so macOS scales the bitmap to fit regardless of the PNG's pixel dimensions
/// or DPI metadata. (A single bitmap is scaled, not resolution-selected —
/// crisp @1x/@2x selection would need multiple `NSImageRep`s, overkill for a
/// monochrome template glyph this small.)
pub(crate) fn set_png_icon(
    item: &NSStatusItem,
    mtm: MainThreadMarker,
    png_2x: &[u8],
    fallback_title: &str,
) {
    let Some(button) = item.button(mtm) else {
        return;
    };
    let data = NSData::with_bytes(png_2x);
    match NSImage::initWithData(NSImage::alloc(), &data) {
        Some(image) => {
            image.setSize(objc2_foundation::NSSize::new(
                STATUS_ICON_POINTS,
                STATUS_ICON_POINTS,
            ));
            image.setTemplate(true);
            button.setImage(Some(&image));
        }
        None => button.setTitle(&NSString::from_str(fallback_title)),
    }
}

/// Append a separator to `menu`.
pub(crate) fn add_separator(menu: &NSMenu, mtm: MainThreadMarker) {
    menu.addItem(&NSMenuItem::separatorItem(mtm));
}
