//! Thin `objc2` wrappers over the macOS `NSStatusItem` / `NSMenu` primitives,
//! used by [`crate::tray`] to host the menu-bar item from the headless agent.
//!
//! Ownership is a value: every object is a [`Retained<T>`] that releases on
//! `Drop`, so the issue-#99 `CFString` leak (the old raw-`id` path) can't be
//! written. The only `unsafe` calls — `initWithTitle:action:keyEquivalent:` and
//! `setTarget:` (raw selector + a *weak* target reference), and the
//! attributed-string calls whose attribute dictionaries objc2 cannot type —
//! are wrapped here.

#![expect(
    unsafe_code,
    reason = "the Objective-C calls objc2 marks unsafe (init-with-action, set-target, text attributes) are wrapped here"
)]

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::runtime::{AnyObject, Bool, Sel};
use objc2::{
    AnyThread, ClassType, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSAttributedStringAttachmentConveniences, NSAttributedStringNSStringDrawing, NSBezierPath,
    NSColor, NSCompositingOperation, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSGraphicsContext, NSImage, NSImageSymbolConfiguration, NSMenu, NSMenuItem,
    NSMutableParagraphStyle, NSParagraphStyle, NSParagraphStyleAttributeName, NSStatusBar,
    NSStatusItem, NSTextAlignment, NSTextAttachment, NSTextTab,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSCopying, NSData, NSDictionary,
    NSMutableAttributedString, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
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

/// Take `item` out of the system status bar. Dropping the last [`Retained`]
/// alone would not: the status bar keeps its own reference.
pub(crate) fn remove_status_item(item: &NSStatusItem) {
    NSStatusBar::systemStatusBar().removeStatusItem(item);
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

/// The diameter of the attention mark in the icon's top-right corner, and the
/// width of the clear ring that sets it apart from the glyph, in points.
const MARK_POINTS: f64 = 6.0;
const MARK_RING_POINTS: f64 = 1.5;

/// Set a custom PNG as the status-item icon (template image). Pass the @2x PNG
/// data; the image's logical size is pinned to the fixed menu-bar point size,
/// so macOS scales the bitmap to fit regardless of the PNG's pixel dimensions
/// or DPI metadata. (A single bitmap is scaled, not resolution-selected —
/// crisp @1x/@2x selection would need multiple `NSImageRep`s, overkill for a
/// monochrome template glyph this small.)
///
/// With `attention`, a dot in the top-right corner marks the glyph. It is
/// drawn into the same template, so the system tints it with the glyph.
pub(crate) fn set_png_icon(
    item: &NSStatusItem,
    mtm: MainThreadMarker,
    png_2x: &[u8],
    attention: bool,
    fallback_title: &str,
) {
    let Some(button) = item.button(mtm) else {
        return;
    };
    let data = NSData::with_bytes(png_2x);
    match NSImage::initWithData(NSImage::alloc(), &data) {
        Some(glyph) => {
            let size = NSSize::new(STATUS_ICON_POINTS, STATUS_ICON_POINTS);
            glyph.setSize(size);
            let image = if attention {
                marked(glyph, size)
            } else {
                glyph
            };
            image.setTemplate(true);
            button.setImage(Some(&image));
        }
        None => button.setTitle(&NSString::from_str(fallback_title)),
    }
}

/// `glyph` with the attention dot in its top-right corner, cut free of the
/// glyph by a clear ring. Drawn on demand, so it stays sharp at any scale.
fn marked(glyph: Retained<NSImage>, size: NSSize) -> Retained<NSImage> {
    let draw = RcBlock::new(move |rect: NSRect| {
        glyph.drawInRect(rect);
        let mark = NSRect::new(
            NSPoint::new(size.width - MARK_POINTS, size.height - MARK_POINTS),
            NSSize::new(MARK_POINTS, MARK_POINTS),
        );
        let ring = NSRect::new(
            NSPoint::new(
                mark.origin.x - MARK_RING_POINTS,
                mark.origin.y - MARK_RING_POINTS,
            ),
            NSSize::new(
                MARK_POINTS + 2.0 * MARK_RING_POINTS,
                MARK_POINTS + 2.0 * MARK_RING_POINTS,
            ),
        );
        if let Some(context) = NSGraphicsContext::currentContext() {
            context.setCompositingOperation(NSCompositingOperation::Clear);
            NSBezierPath::bezierPathWithOvalInRect(ring).fill();
            context.setCompositingOperation(NSCompositingOperation::SourceOver);
        }
        NSColor::blackColor().setFill();
        NSBezierPath::bezierPathWithOvalInRect(mark).fill();
        Bool::YES
    });
    NSImage::imageWithSize_flipped_drawingHandler(size, false, &draw)
}

/// A disabled item that only shows `title`.
pub(crate) fn info_item(mtm: MainThreadMarker, title: &str) -> Retained<NSMenuItem> {
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str(title));
    item.setEnabled(false);
    item
}

/// A section header titled `title`: AppKit's own from macOS 14, which added
/// it, and a disabled item before.
pub(crate) fn section_header(mtm: MainThreadMarker, title: &str) -> Retained<NSMenuItem> {
    if NSMenuItem::class()
        .class_method(sel!(sectionHeaderWithTitle:))
        .is_some()
    {
        NSMenuItem::sectionHeaderWithTitle(&NSString::from_str(title), mtm)
    } else {
        info_item(mtm, title)
    }
}

/// The SF Symbol `name`, described as `description` to VoiceOver (`None` for
/// one that only decorates its item's title), or `None` on a macOS too old to
/// have it.
pub(crate) fn symbol(name: &str, description: Option<&str>) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        description.map(NSString::from_str).as_deref(),
    )
}

/// `symbol` drawn in `colors`, layer by layer.
pub(crate) fn tinted(symbol: &NSImage, colors: &[&NSColor]) -> Retained<NSImage> {
    let configuration =
        NSImageSymbolConfiguration::configurationWithPaletteColors(&NSArray::from_slice(colors));
    symbol
        .imageWithSymbolConfiguration(&configuration)
        .unwrap_or_else(|| symbol.copy())
}

/// The trailing part of a menu item's title: an optional glyph, then text,
/// right-aligned and in the secondary label colour — a battery reading, a
/// state, what clicking does.
pub(crate) struct Trailing {
    glyph: Option<Retained<NSImage>>,
    text: String,
}

impl Trailing {
    /// Trailing `text` alone.
    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self {
            glyph: None,
            text: text.into(),
        }
    }

    /// Trailing `text` after `glyph`, when there is one.
    pub(crate) fn glyph(glyph: Option<Retained<NSImage>>, text: impl Into<String>) -> Self {
        Self {
            glyph,
            text: text.into(),
        }
    }

    fn width(&self, font: &NSFont) -> f64 {
        let glyph = self
            .glyph
            .as_ref()
            .map_or(0.0, |glyph| glyph.size().width + GLYPH_GAP);
        glyph + text_width(&self.text, font)
    }
}

/// The gap before a trailing part, and between its glyph and its text, in
/// points.
const TRAILING_GAP: f64 = 24.0;
const GLYPH_GAP: f64 = 4.0;

/// One menu's items whose titles end in a [`Trailing`] part. Their titles are
/// set together by [`Self::apply`], once the widest is known, so the trailing
/// parts line up on one right edge.
#[derive(Default)]
pub(crate) struct TrailingTitles(Vec<(Retained<NSMenuItem>, String, Trailing)>);

impl TrailingTitles {
    /// Title `item` with `title` followed by `trailing`, at [`Self::apply`].
    pub(crate) fn push(&mut self, item: &NSMenuItem, title: String, trailing: Trailing) {
        self.0.push((item.retain(), title, trailing));
    }

    /// Set every pushed title.
    pub(crate) fn apply(self) {
        let font = menu_font();
        let tab = self
            .0
            .iter()
            .map(|(_, title, trailing)| {
                text_width(title, &font) + TRAILING_GAP + trailing.width(&font)
            })
            .fold(0.0, f64::max);
        let paragraph = NSMutableParagraphStyle::new();
        // SAFETY: the options dictionary is empty, so its generic types hold
        // trivially.
        let tab_stop = unsafe {
            NSTextTab::initWithTextAlignment_location_options(
                NSTextTab::alloc(),
                NSTextAlignment::Right,
                tab,
                &NSDictionary::new(),
            )
        };
        paragraph.setTabStops(Some(&NSArray::from_retained_slice(&[tab_stop])));
        let label = attributes(&font, &paragraph, None);
        let secondary = NSColor::secondaryLabelColor();
        let detail = attributes(&font, &paragraph, Some(&secondary));
        for (item, title, trailing) in self.0 {
            let text = NSMutableAttributedString::new();
            text.appendAttributedString(&attributed(&format!("{title}\t"), &label));
            if let Some(glyph) = &trailing.glyph {
                let attachment = NSTextAttachment::new();
                attachment.setImage(Some(glyph));
                let size = glyph.size();
                // Centre the glyph on the text's capitals rather than sitting
                // it on the baseline.
                attachment.setBounds(NSRect::new(
                    NSPoint::new(0.0, (font.capHeight() - size.height) / 2.0),
                    size,
                ));
                // SAFETY: `detail` maps each key to the type AppKit documents
                // for it (`attributes`).
                let glyph = unsafe {
                    NSAttributedString::attributedStringWithAttachment_attributes(
                        &attachment,
                        &detail,
                    )
                };
                text.appendAttributedString(&glyph);
                text.appendAttributedString(&attributed(" ", &detail));
            }
            text.appendAttributedString(&attributed(&trailing.text, &detail));
            item.setAttributedTitle(Some(&text));
        }
    }
}

/// The font menus draw their items in.
fn menu_font() -> Retained<NSFont> {
    // Size 0 asks for the menu's standard size.
    NSFont::menuFontOfSize(0.0)
}

fn text_width(text: &str, font: &NSFont) -> f64 {
    let paragraph = NSParagraphStyle::defaultParagraphStyle();
    attributed(text, &attributes(font, &paragraph, None))
        .size()
        .width
}

type Attributes = NSDictionary<NSAttributedStringKey, AnyObject>;

/// Text attributes setting `font`, `paragraph` and, when given, `color`. Each
/// key maps to the type AppKit documents for it, which is what the `unsafe`
/// calls taking these attributes rely on.
fn attributes(
    font: &NSFont,
    paragraph: &NSParagraphStyle,
    color: Option<&NSColor>,
) -> Retained<Attributes> {
    // SAFETY: AppKit exports each key as an immutable process-lifetime
    // constant.
    let (font_key, paragraph_key, color_key) = unsafe {
        (
            NSFontAttributeName,
            NSParagraphStyleAttributeName,
            NSForegroundColorAttributeName,
        )
    };
    let mut keys = vec![font_key, paragraph_key];
    let mut values: Vec<&AnyObject> = vec![font.as_ref(), paragraph.as_ref()];
    if let Some(color) = color {
        keys.push(color_key);
        values.push(color.as_ref());
    }
    NSDictionary::from_slices(&keys, &values)
}

fn attributed(text: &str, attributes: &Attributes) -> Retained<NSAttributedString> {
    // SAFETY: `attributes` maps each key to the type AppKit documents for it
    // (`attributes`).
    unsafe { NSAttributedString::new_with_attributes(&NSString::from_str(text), attributes) }
}

/// Append a separator to `menu`.
pub(crate) fn add_separator(menu: &NSMenu, mtm: MainThreadMarker) {
    menu.addItem(&NSMenuItem::separatorItem(mtm));
}

/// Deliver a persistent battery alert through UserNotifications.
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
