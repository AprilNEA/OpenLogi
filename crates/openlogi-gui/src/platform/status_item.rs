//! Thin `objc2` wrappers over the macOS `NSStatusItem` / `NSMenu` primitives.
//!
//! GPUI exposes no status-bar API, so OpenLogi drives `NSStatusItem` directly.
//! Where the old `cocoa`/`objc` 0.x path modelled every object as a raw,
//! hand-retained `id` (the source of the issue-#99 `CFString` leak), `objc2`
//! gives [`Retained<T>`]: ownership is a value, so each object releases itself
//! on `Drop` and a "+1 with no owner" can't be written. The only `unsafe`
//! Objective-C calls left — `initWithTitle:action:keyEquivalent:` and
//! `setTarget:`, which take a raw selector and store a *weak* reference — are
//! wrapped here behind safe functions; [`super::tray`] builds the OpenLogi menu
//! on top.

#![expect(
    unsafe_code,
    reason = "the two Objective-C calls objc2 marks unsafe (init-with-action, set-target) are wrapped here"
)]

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuItem, NSStatusBar,
    NSStatusItem,
};
use objc2_foundation::NSString;

/// `NSVariableStatusItemLength` — a status item sized to its content.
const VARIABLE_LENGTH: f64 = -1.0;

/// macOS application activation policy values used by OpenLogi.
#[derive(Clone, Copy)]
pub(super) enum ActivationPolicy {
    /// Standard Dock + app menu-bar presence.
    Regular,
    /// Hide from Dock/app menu bar while keeping the status item alive.
    Accessory,
}

/// Create and return a variable-width status item. The returned [`Retained`]
/// owns it; the tray keeps it for the app's lifetime.
pub(super) fn create_status_item() -> Retained<NSStatusItem> {
    NSStatusBar::systemStatusBar().statusItemWithLength(VARIABLE_LENGTH)
}

/// Remove `item` from the system status bar during teardown.
pub(super) fn remove_status_item(item: &NSStatusItem) {
    NSStatusBar::systemStatusBar().removeStatusItem(item);
}

/// Use an SF Symbol as the status-item icon, falling back to a text title.
pub(super) fn set_symbol_icon(
    item: &NSStatusItem,
    mtm: MainThreadMarker,
    symbol: &str,
    description: &str,
    fallback_title: &str,
) {
    let Some(button) = item.button(mtm) else {
        return;
    };
    match NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(symbol),
        Some(&NSString::from_str(description)),
    ) {
        Some(image) => {
            image.setTemplate(true);
            button.setImage(Some(&image));
        }
        None => button.setTitle(&NSString::from_str(fallback_title)),
    }
}

/// Create a menu with AppKit auto-enabling disabled (OpenLogi manages item
/// state itself).
pub(super) fn new_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    menu
}

/// Create a disabled, title-only menu item (used for the device rows).
pub(super) fn new_disabled_item(mtm: MainThreadMarker, title: &str) -> Retained<NSMenuItem> {
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str(title));
    item.setEnabled(false);
    item
}

/// Create an action item that sends `action` to `target` when clicked.
///
/// `target` is stored as a *weak* reference by AppKit, so the caller must keep
/// it alive for as long as the item can be clicked (the tray holds the
/// `Retained` target for the app's lifetime).
pub(super) fn new_action_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Sel,
    target: &AnyObject,
) -> Retained<NSMenuItem> {
    // SAFETY: `initWithTitle:action:keyEquivalent:` is NSMenuItem's designated
    // initializer; the two `NSString`s outlive the call and `action` is a
    // selector `target` responds to (wired up by `setTarget:` below).
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(""),
        )
    };
    // SAFETY: `target` is a live Objective-C object that responds to `action`.
    // NSMenuItem keeps only a weak reference, so the caller retains `target`
    // (see the doc comment) — there is no dangling-target window.
    unsafe { item.setTarget(Some(target)) };
    item
}

/// Append a separator to `menu`.
pub(super) fn add_separator(menu: &NSMenu, mtm: MainThreadMarker) {
    menu.addItem(&NSMenuItem::separatorItem(mtm));
}

/// Set the process-wide AppKit activation policy (Dock + menu-bar presence).
pub(super) fn set_activation_policy(mtm: MainThreadMarker, policy: ActivationPolicy) {
    let raw = match policy {
        ActivationPolicy::Regular => NSApplicationActivationPolicy::Regular,
        ActivationPolicy::Accessory => NSApplicationActivationPolicy::Accessory,
    };
    let _ = NSApplication::sharedApplication(mtm).setActivationPolicy(raw);
}
