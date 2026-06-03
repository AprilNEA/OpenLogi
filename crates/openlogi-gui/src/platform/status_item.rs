//! Thin macOS `NSStatusItem` / `NSMenu` wrapper used by the OpenLogi tray.

#![expect(
    unsafe_code,
    reason = "Cocoa NSStatusItem/NSMenu FFI; GPUI has no menu-bar API"
)]

use std::sync::Once;

use cocoa::base::{NO, YES, id, nil};
use cocoa::foundation::NSString;
use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};

/// Objective-C action callback signature used by status-item menu entries.
pub(super) type ActionCallback = extern "C" fn(&Object, Sel, id);

/// macOS application activation policy values used by OpenLogi.
#[derive(Clone, Copy)]
pub(super) enum ActivationPolicy {
    /// Standard Dock + app menu-bar presence.
    Regular,
    /// Hide from Dock/app menu bar while keeping the status item alive.
    Accessory,
}

impl ActivationPolicy {
    fn as_raw(self) -> i64 {
        match self {
            Self::Regular => 0,
            Self::Accessory => 1,
        }
    }
}

/// Set the process-wide AppKit activation policy.
pub(super) fn set_activation_policy(policy: ActivationPolicy) {
    let app: id = unsafe {
        // SAFETY: `NSApplication.sharedApplication` is a process-wide AppKit
        // singleton and is available on the main thread after GPUI starts.
        msg_send![class!(NSApplication), sharedApplication]
    };
    unsafe {
        // SAFETY: `app` is the shared `NSApplication`, and the integer value is
        // one of AppKit's documented activation policies.
        let _: () = msg_send![app, setActivationPolicy: policy.as_raw()];
    }
}

/// A retained `NSStatusItem` handle.
#[derive(Clone, Copy)]
pub(super) struct StatusItem(usize);

impl StatusItem {
    /// Create and retain a variable-width status item.
    pub(super) fn new() -> Self {
        const VARIABLE_LENGTH: f64 = -1.0;

        let status_bar: id = unsafe {
            // SAFETY: `NSStatusBar.systemStatusBar` returns the process-wide
            // status bar when called from the main AppKit thread.
            msg_send![class!(NSStatusBar), systemStatusBar]
        };
        let status_item: id = unsafe {
            // SAFETY: `status_bar` is AppKit's shared `NSStatusBar`; variable
            // length is the documented `NSVariableStatusItemLength` sentinel.
            msg_send![status_bar, statusItemWithLength: VARIABLE_LENGTH]
        };
        unsafe {
            // SAFETY: the newly-created status item is a valid Objective-C
            // object; retaining keeps it alive for the app lifetime.
            let _: id = msg_send![status_item, retain];
        }
        Self(status_item as usize)
    }

    /// Show or hide the status item without tearing it down.
    pub(super) fn set_visible(&self, visible: bool) {
        let flag = if visible { YES } else { NO };
        unsafe {
            // SAFETY: `self.raw()` is the retained `NSStatusItem` created by
            // `StatusItem::new`; `setVisible:` accepts an Objective-C BOOL.
            let _: () = msg_send![self.raw(), setVisible: flag];
        }
    }

    /// Remove the status item from the system status bar.
    pub(super) fn remove_from_status_bar(&self) {
        let status_bar: id = unsafe {
            // SAFETY: `NSStatusBar.systemStatusBar` returns the process-wide
            // status bar when called from the main AppKit thread.
            msg_send![class!(NSStatusBar), systemStatusBar]
        };
        unsafe {
            // SAFETY: `self.raw()` is the retained `NSStatusItem` created by
            // `StatusItem::new`; removing it from the owning status bar is the
            // documented AppKit teardown path for menu-bar extras.
            let _: () = msg_send![status_bar, removeStatusItem: self.raw()];
        }
    }

    /// Attach a menu to the status item.
    pub(super) fn set_menu(&self, menu: Menu) {
        unsafe {
            // SAFETY: both handles are retained AppKit objects created by this
            // module, and `setMenu:` does not take Rust ownership.
            let _: () = msg_send![self.raw(), setMenu: menu.raw()];
        }
    }

    /// Use an SF Symbol as the status-item icon, falling back to a text title.
    pub(super) fn set_symbol_icon(&self, symbol: &str, description: &str, fallback_title: &str) {
        let button: id = unsafe {
            // SAFETY: `self.raw()` is a valid retained `NSStatusItem`; AppKit
            // returns its button or nil on unsupported versions.
            msg_send![self.raw(), button]
        };
        let image: id = unsafe {
            // SAFETY: the selector is an AppKit constructor; the arguments are
            // temporary `NSString`s valid for the duration of the message send.
            msg_send![class!(NSImage), imageWithSystemSymbolName: nsstring(symbol) accessibilityDescription: nsstring(description)]
        };
        if image == nil {
            unsafe {
                // SAFETY: `button` is the status-item button returned by
                // AppKit; setting a text title is valid when no image exists.
                let _: () = msg_send![button, setTitle: nsstring(fallback_title)];
            }
        } else {
            unsafe {
                // SAFETY: `image` is a valid `NSImage`; `setTemplate:` accepts
                // an Objective-C BOOL and does not transfer ownership.
                let _: () = msg_send![image, setTemplate: YES];
            }
            unsafe {
                // SAFETY: `button` and `image` are AppKit objects; `setImage:`
                // stores the image according to AppKit ownership rules.
                let _: () = msg_send![button, setImage: image];
            }
        }
    }

    fn raw(&self) -> id {
        self.0 as id
    }
}

/// A retained `NSMenu` handle.
#[derive(Clone, Copy)]
pub(super) struct Menu(usize);

impl Menu {
    /// Create and retain a menu with AppKit auto-enabling disabled.
    pub(super) fn new() -> Self {
        let menu: id = unsafe {
            // SAFETY: `NSMenu.new` creates a valid AppKit menu on the main
            // thread.
            msg_send![class!(NSMenu), new]
        };
        unsafe {
            // SAFETY: `menu` is a valid Objective-C object; retaining keeps it
            // alive for the status item's lifetime.
            let _: id = msg_send![menu, retain];
        }
        unsafe {
            // SAFETY: `menu` is a valid `NSMenu`; disabling auto-enabling is a
            // standard AppKit property mutation.
            let _: () = msg_send![menu, setAutoenablesItems: NO];
        }
        Self(menu as usize)
    }

    /// Append a menu item.
    pub(super) fn add_item(&self, item: MenuItem) {
        unsafe {
            // SAFETY: `self` and `item` are valid AppKit objects created by
            // this module; `addItem:` retains according to AppKit rules.
            let _: () = msg_send![self.raw(), addItem: item.raw()];
        }
    }

    /// Append a separator item.
    pub(super) fn add_separator(&self) {
        let separator: id = unsafe {
            // SAFETY: `NSMenuItem.separatorItem` returns a valid autoreleased
            // separator item suitable for adding to a menu.
            msg_send![class!(NSMenuItem), separatorItem]
        };
        unsafe {
            // SAFETY: `self.raw()` is a valid `NSMenu`, and `separator` is a
            // valid `NSMenuItem` returned by AppKit.
            let _: () = msg_send![self.raw(), addItem: separator];
        }
    }

    fn raw(&self) -> id {
        self.0 as id
    }
}

/// A retained `NSMenuItem` handle.
#[derive(Clone, Copy)]
pub(super) struct MenuItem(usize);

impl MenuItem {
    /// Create a disabled title-only item.
    pub(super) fn disabled(title: &str) -> Self {
        let item: id = unsafe {
            // SAFETY: `NSMenuItem.new` creates a valid menu item on the main
            // AppKit thread.
            msg_send![class!(NSMenuItem), new]
        };
        unsafe {
            // SAFETY: `item` is a valid `NSMenuItem`; the temporary `NSString`
            // is valid for the duration of the message send.
            let _: () = msg_send![item, setTitle: nsstring(title)];
        }
        unsafe {
            // SAFETY: `item` is a valid `NSMenuItem`; `setEnabled:` accepts an
            // Objective-C BOOL.
            let _: () = msg_send![item, setEnabled: NO];
        }
        Self(item as usize)
    }

    /// Create an action item targeting the supplied Objective-C receiver.
    pub(super) fn action(title: &str, action: Sel, target: &ActionTarget) -> Self {
        let item: id = unsafe {
            // SAFETY: `NSMenuItem.alloc` allocates a menu item object for the
            // following initializer.
            msg_send![class!(NSMenuItem), alloc]
        };
        let item: id = unsafe {
            // SAFETY: `item` is allocated, `action` is registered on `target`,
            // and the `NSString` arguments live for this message send.
            msg_send![item, initWithTitle: nsstring(title) action: action keyEquivalent: nsstring("")]
        };
        unsafe {
            // SAFETY: `item` is an initialized `NSMenuItem`, and `target.raw()`
            // is a retained Objective-C target object.
            let _: () = msg_send![item, setTarget: target.raw()];
        }
        Self(item as usize)
    }

    /// Replace the menu item title.
    pub(super) fn set_title(&self, title: &str) {
        unsafe {
            // SAFETY: `self.raw()` is a valid `NSMenuItem`; the temporary
            // `NSString` is valid for the duration of the message send.
            let _: () = msg_send![self.raw(), setTitle: nsstring(title)];
        }
    }

    fn raw(&self) -> id {
        self.0 as id
    }
}

/// A retained Objective-C target object for menu actions.
pub(super) struct ActionTarget(usize);

impl ActionTarget {
    /// Register the target class once and create a retained target instance.
    pub(super) fn new(class_name: &'static str, methods: &[(Sel, ActionCallback)]) -> Self {
        register_target_class(class_name, methods);
        let target_cls = Class::get(class_name).unwrap_or_else(|| class!(NSObject));
        let target: id = unsafe {
            // SAFETY: `target_cls` is either the registered target class or
            // NSObject fallback; `new` returns a valid Objective-C object.
            msg_send![target_cls, new]
        };
        // NSMenuItem keeps only a weak reference to its target — retain it so it
        // outlives the caller and the action callbacks stay valid.
        unsafe {
            // SAFETY: `target` is a valid Objective-C object created above;
            // retaining intentionally leaks it for the app lifetime.
            let _: id = msg_send![target, retain];
        }
        Self(target as usize)
    }

    fn raw(&self) -> id {
        self.0 as id
    }
}

fn register_target_class(class_name: &'static str, methods: &[(Sel, ActionCallback)]) {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        if let Some(mut decl) = ClassDecl::new(class_name, class!(NSObject)) {
            for (selector, callback) in methods {
                unsafe {
                    // SAFETY: each callback uses the Objective-C method ABI and
                    // matches the selector signature used by `NSMenuItem`.
                    decl.add_method(*selector, *callback);
                }
            }
            decl.register();
        }
    });
}

fn nsstring(s: &str) -> id {
    unsafe {
        // SAFETY: `NSString::alloc(nil).init_str` constructs an Objective-C
        // string from a Rust `&str`; callers use it immediately in msg_send.
        NSString::alloc(nil).init_str(s)
    }
}
