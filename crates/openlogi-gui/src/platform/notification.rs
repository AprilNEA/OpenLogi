//! Observe distributed notifications from the agent's tray menu.
//!
//! The agent posts `org.openlogi.gui-command` with a `command` key in
//! `userInfo` when the user clicks a GUI-directed tray item while the GUI is
//! already running. The notification observer pushes the command into a
//! channel consumed by the GPUI event loop. macOS-only; on other platforms
//! this module is a no-op.

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "objc2 calls: define_class super-init, addObserver:selector:, NSDictionary access"
)]
mod inner {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_foundation::{
        NSDistributedNotificationCenter, NSNotification, NSNotificationName,
        NSNotificationSuspensionBehavior, NSString,
    };
    use tokio::sync::mpsc;

    use super::GuiCommand;

    pub const NOTIFICATION_NAME: &str = "org.openlogi.gui-command";

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and the observer
        // does not implement Drop.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "OpenLogiNotificationObserver"]
        #[ivars = mpsc::UnboundedSender<GuiCommand>]
        struct NotificationObserver;

        // SAFETY: the selector is registered once and matches the objc
        // signature `- (void)handleCommand:(NSNotification *)note`.
        impl NotificationObserver {
            #[unsafe(method(handleCommand:))]
            fn handle_command(&self, notification: &NSNotification) {
                let Some(user_info) = notification.userInfo() else {
                    return;
                };
                let key: Retained<AnyObject> =
                    Retained::into_super(Retained::into_super(NSString::from_str("command")));
                let Some(value) = user_info.objectForKey(&key) else {
                    return;
                };
                // SAFETY: the value is an NSString (set by the agent).
                // SAFETY: the value is an NSString (set by the agent).
                let ns_str: &NSString = unsafe { &*Retained::as_ptr(&value).cast::<NSString>() };
                let value = ns_str.to_string();
                if let Some(cmd) = GuiCommand::parse(&value) {
                    let _ = self.ivars().send(cmd);
                }
            }
        }
    );

    /// Opaque handle keeping the notification observer alive. Drop unsubscribes.
    pub struct NotificationGuard {
        _observer: Retained<NotificationObserver>,
        _center: Retained<NSDistributedNotificationCenter>,
    }

    /// Register the distributed-notification observer. The returned guard
    /// must be kept alive for the app's lifetime — dropping it unsubscribes.
    pub fn subscribe(
        mtm: MainThreadMarker,
        tx: mpsc::UnboundedSender<GuiCommand>,
    ) -> NotificationGuard {
        let observer: Retained<NotificationObserver> = {
            let this = NotificationObserver::alloc(mtm).set_ivars(tx);
            // SAFETY: standard two-phase NSObject init.
            unsafe { msg_send![super(this), init] }
        };
        let center = NSDistributedNotificationCenter::defaultCenter();
        let name = NSNotificationName::from_str(NOTIFICATION_NAME);
        // SAFETY: observer + selector are valid and match.
        unsafe {
            center.addObserver_selector_name_object_suspensionBehavior(
                &observer,
                sel!(handleCommand:),
                Some(&name),
                None,
                NSNotificationSuspensionBehavior::DeliverImmediately,
            );
        }
        NotificationGuard {
            _observer: observer,
            _center: center,
        }
    }
}

/// A GUI action requested by the agent's tray menu.
#[derive(Clone, Debug)]
pub enum GuiCommand {
    OpenSettings,
    OpenAbout,
    CheckForUpdates,
}

impl GuiCommand {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open-settings" => Some(Self::OpenSettings),
            "open-about" => Some(Self::OpenAbout),
            "check-for-updates" => Some(Self::CheckForUpdates),
            _ => None,
        }
    }
}

#[cfg(target_os = "macos")]
pub use inner::subscribe;
