//! Thin `objc2` wrappers over the macOS `NSStatusItem` / `NSMenu` primitives,
//! used by [`crate::tray`] to host the menu-bar item from the headless agent.
//!
//! Ownership is a value: every object is a [`Retained<T>`] that releases on
//! `Drop`, so the issue-#99 `CFString` leak (the old raw-`id` path) can't be
//! written. Native drawing, selectors and UserNotifications callbacks stay here.

#![expect(
    unsafe_code,
    reason = "native menu subclassing, typed drawing attributes and notification callback pointers"
)]

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem};
use objc2_foundation::{NSData, NSString};

use objc2::{DefinedClass, define_class, msg_send};
use objc2_app_kit::{
    NSAccessibility, NSAttributedStringNSStringDrawing, NSAutoresizingMaskOptions, NSBezierPath,
    NSColor, NSEvent, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSView,
};
use objc2_foundation::{NSAttributedString, NSDictionary, NSPoint, NSRect, NSSize};
use openlogi_core::battery::Severity;

use objc2::runtime::{NSObject, ProtocolObject};
use objc2_foundation::NSObjectProtocol;
use objc2_user_notifications::{
    UNNotification, UNNotificationPresentationOptions, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

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

/// AppKit ignores status-button contentTintColor. Warning colors therefore
/// use a non-template image drawn from the original asset, never a prior tint.
pub(crate) fn set_warning_tint(
    item: &NSStatusItem,
    mtm: MainThreadMarker,
    source_png: &[u8],
    severity: Severity,
) {
    let color = match severity {
        Severity::Normal => {
            set_png_icon(item, mtm, source_png, "OpenLogi");
            return;
        }
        Severity::Low => NSColor::systemOrangeColor(),
        Severity::Critical => NSColor::systemRedColor(),
    };
    let Some(button) = item.button(mtm) else {
        return;
    };
    let data = NSData::with_bytes(source_png);
    let Some(source) = NSImage::initWithData(NSImage::alloc(), &data) else {
        button.setTitle(&NSString::from_str("OpenLogi"));
        return;
    };
    let size = NSSize::new(STATUS_ICON_POINTS, STATUS_ICON_POINTS);
    let drawing = block2::RcBlock::new(move |rect: NSRect| {
        use objc2_app_kit::{NSCompositingOperation, NSRectFillUsingOperation};
        source.drawInRect_fromRect_operation_fraction(
            rect,
            NSRect::new(NSPoint::new(0.0, 0.0), source.size()),
            NSCompositingOperation::Copy,
            1.0,
        );
        color.setFill();
        NSRectFillUsingOperation(rect, NSCompositingOperation::SourceIn);
        objc2::runtime::Bool::YES
    });
    let image = NSImage::imageWithSize_flipped_drawingHandler(size, false, &drawing);
    image.setTemplate(false);
    button.setImage(Some(&image));
}

fn text(value: &str, font: &NSFont, color: &NSColor) -> Retained<NSAttributedString> {
    // SAFETY: AppKit's attribute keys are process-lifetime constants; each
    // value has the type required by its corresponding key.
    unsafe {
        let attributes = NSDictionary::from_slices(
            &[NSFontAttributeName, NSForegroundColorAttributeName],
            &[font as &AnyObject, color as &AnyObject],
        );
        NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &NSString::from_str(value),
            Some(&attributes),
        )
    }
}

struct BatteryRowIvars {
    device: crate::battery::DeviceBattery,
}

define_class!(
    // SAFETY: NSView subclass with owned Rust ivars and no custom Drop.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "OpenLogiBatteryMenuRow"]
    #[ivars = BatteryRowIvars]
    struct BatteryRow;

    impl BatteryRow {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool { true }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool { true }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool { true }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw_contents();
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let bounds = self.bounds();
            if point.x >= 0.0 && point.y >= 0.0
                && point.x <= bounds.size.width && point.y <= bounds.size.height {
                self.activate();
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let key = event.charactersIgnoringModifiers().map(|value| value.to_string());
            if matches!(key.as_deref(), Some("\r" | " ")) {
                self.activate();
            } else {
                // SAFETY: forward the original AppKit event to NSView.
                unsafe { let _: () = msg_send![super(self), keyDown: event]; }
            }
        }

        #[unsafe(method(isAccessibilityElement))]
        fn is_accessibility_element(&self) -> bool { true }

        #[unsafe(method_id(accessibilityRole))]
        fn accessibility_role(&self) -> Retained<NSString> {
            NSString::from_str("AXMenuItem")
        }

        #[unsafe(method_id(accessibilityLabel))]
        fn accessibility_label(&self) -> Retained<NSString> {
            NSString::from_str(&self.ivars().device.accessible_label())
        }

        #[unsafe(method(accessibilityPerformPress))]
        fn accessibility_perform_press(&self) -> bool {
            self.activate();
            true
        }
    }
);

impl BatteryRow {
    fn activate(&self) {
        if let Some(item) = self.enclosingMenuItem()
            // SAFETY: the enclosing menu owns this item throughout activation.
            && let Some(menu) = unsafe { item.menu() }
        {
            let index = menu.indexOfItem(&item);
            if index >= 0 {
                menu.cancelTracking();
                menu.performActionForItemAtIndex(index);
            }
        }
    }

    fn draw_contents(&self) {
        let bounds = self.bounds();
        let selected = self
            .enclosingMenuItem()
            .is_some_and(|item| item.isHighlighted());
        if selected {
            NSColor::selectedContentBackgroundColor().setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                NSRect::new(
                    NSPoint::new(5.0, 1.0),
                    NSSize::new(bounds.size.width - 10.0, bounds.size.height - 2.0),
                ),
                4.0,
                4.0,
            )
            .fill();
        }
        let foreground = if selected {
            NSColor::selectedMenuItemTextColor()
        } else {
            NSColor::labelColor()
        };
        let device = &self.ivars().device;
        let font = NSFont::menuFontOfSize(0.0);
        text(&device.name, &font, &foreground).drawInRect(NSRect::new(
            NSPoint::new(20.0, 5.0),
            NSSize::new(bounds.size.width - 98.0, 20.0),
        ));
        let color = if device.charging {
            NSColor::systemGreenColor()
        } else {
            match device.severity {
                Severity::Normal => foreground.clone(),
                Severity::Low => NSColor::systemOrangeColor(),
                Severity::Critical => NSColor::systemRedColor(),
            }
        };
        let x = bounds.size.width - 66.0;
        let y = (bounds.size.height - 18.0) / 2.0;
        color.setStroke();
        let outline = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            NSRect::new(NSPoint::new(x, y), NSSize::new(44.0, 18.0)),
            3.0,
            3.0,
        );
        outline.setLineWidth(1.2);
        outline.stroke();
        color.setFill();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            NSRect::new(NSPoint::new(x + 45.0, y + 5.0), NSSize::new(2.0, 8.0)),
            1.0,
            1.0,
        )
        .fill();
        let value = device
            .percentage
            .map_or_else(|| "?".to_owned(), |value| format!("{value}%"));
        // Keep the small percentage readable on native translucent backgrounds;
        // only the battery outline carries the optional severity color.
        let number = text(&value, &NSFont::boldSystemFontOfSize(10.0), &foreground);
        let size = number.size();
        let area_width = if device.charging { 33.0 } else { 44.0 };
        number.drawAtPoint(NSPoint::new(
            x + (area_width - size.width) / 2.0,
            y + (18.0 - size.height) / 2.0,
        ));
        if device.charging {
            foreground.setFill();
            let bolt = NSBezierPath::bezierPath();
            bolt.moveToPoint(NSPoint::new(x + 39.0, y + 3.0));
            for (dx, dy) in [
                (34.0, 10.0),
                (38.0, 10.0),
                (36.0, 15.0),
                (42.0, 7.0),
                (38.0, 7.0),
            ] {
                bolt.lineToPoint(NSPoint::new(x + dx, y + dy));
            }
            bolt.closePath();
            bolt.fill();
        }
    }
}

/// Attach a native menu view, leaving the semantic title and action intact.
pub(crate) fn set_battery_row(
    item: &NSMenuItem,
    mtm: MainThreadMarker,
    device: crate::battery::DeviceBattery,
) {
    item.setAccessibilityLabel(Some(&NSString::from_str(&device.accessible_label())));
    let width = (text(
        &device.name,
        &NSFont::menuFontOfSize(0.0),
        &NSColor::labelColor(),
    )
    .size()
    .width
        + 110.0)
        .clamp(260.0, 440.0);
    let this = BatteryRow::alloc(mtm).set_ivars(BatteryRowIvars { device });
    // SAFETY: initialize the allocated NSView subclass with its designated initializer.
    let view: Retained<BatteryRow> = unsafe {
        msg_send![super(this), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, 28.0))]
    };
    view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    item.setView(Some(&view));
}

/// Ask only when an actual low-battery alert exists. UserNotifications keeps
/// authorization across launches; a denied request does not prompt again.
pub(crate) fn notify_battery(alert: &crate::battery::Alert) {
    let title = alert.title();
    let body = alert.body();
    dispatch2::DispatchQueue::main().exec_async(move || {
        use objc2_foundation::{NSBundle, NSError};
        use objc2_user_notifications::{UNAuthorizationOptions, UNUserNotificationCenter};
        // UNUserNotificationCenter requires an application bundle identity.
        // Bare cargo executables must not attempt its exception-raising API.
        if NSBundle::mainBundle().bundleIdentifier().is_none() {
            tracing::warn!("battery notification unavailable outside an application bundle");
            return;
        }
        NOTIFICATION_DELEGATE.with(|delegate| {
            UNUserNotificationCenter::currentNotificationCenter()
                .setDelegate(Some(ProtocolObject::from_ref(&**delegate)));
        });
        let completion = block2::RcBlock::new(move |granted: objc2::runtime::Bool, error: *mut NSError| {
            if !error.is_null() {
                // SAFETY: UserNotifications lends a valid NSError during this callback.
                let error = unsafe { &*error };
                tracing::warn!(error = %error.localizedDescription(), "battery notification authorization failed");
            } else if granted.as_bool() {
                let title = title.clone();
                let body = body.clone();
                dispatch2::DispatchQueue::main().exec_async(move || deliver_battery_notification(&title, &body));
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert, &completion);
    });
}

fn deliver_battery_notification(title: &str, body: &str) {
    use objc2_foundation::{NSError, NSUUID};
    use objc2_user_notifications::{
        UNMutableNotificationContent, UNNotificationRequest, UNUserNotificationCenter,
    };
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSUUID::UUID().UUIDString(),
        &content,
        None,
    );
    let completion = block2::RcBlock::new(|error: *mut NSError| {
        if !error.is_null() {
            // SAFETY: UserNotifications lends a valid NSError during this callback.
            let error = unsafe { &*error };
            tracing::warn!(error = %error.localizedDescription(), "battery notification delivery failed");
        }
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .addNotificationRequest_withCompletionHandler(&request, Some(&completion));
}

define_class!(
    // SAFETY: NSObject subclass without ivars or custom Drop. The delegate may
    // run on UserNotifications' background queue and has no AppKit state.
    #[unsafe(super(NSObject))]
    #[name = "OpenLogiBatteryNotificationDelegate"]
    struct NotificationDelegate;

    // SAFETY: the superclass provides NSObject's protocol implementation.
    unsafe impl NSObjectProtocol for NotificationDelegate {}

    // SAFETY: all arguments are borrowed only for the callback's duration.
    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion
                .call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List,));
        }
    }
);

thread_local! {
    // The center keeps a weak delegate. Installation and ownership stay on the
    // main queue; callbacks contain no thread-affine state.
    static NOTIFICATION_DELEGATE: Retained<NotificationDelegate> = {
        let this = NotificationDelegate::alloc().set_ivars(());
        // SAFETY: initialize the freshly allocated NSObject subclass.
        unsafe { msg_send![super(this), init] }
    };
}
