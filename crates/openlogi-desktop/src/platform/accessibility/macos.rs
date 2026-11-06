//! macOS display preferences and their native change notification.

use std::{ptr::NonNull, time::Duration};

use block2::RcBlock;
use gpui::{App, Global, Task};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{NSWorkspace, NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};

/// Whether the system permits translucent window backgrounds.
pub(crate) fn allows_glass() -> bool {
    let workspace = NSWorkspace::sharedWorkspace();
    !workspace.accessibilityDisplayShouldReduceTransparency()
        && !workspace.accessibilityDisplayShouldIncreaseContrast()
}

struct Registration {
    center: Retained<NSNotificationCenter>,
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl Drop for Registration {
    #[expect(
        unsafe_code,
        reason = "unregister the token owned by this registration"
    )]
    fn drop(&mut self) {
        let token: &ProtocolObject<dyn NSObjectProtocol> = &self.token;
        // SAFETY: This token was returned by this notification center. It stays
        // retained until removal finishes, and it is removed exactly once.
        unsafe { self.center.removeObserver(token.as_ref()) };
    }
}

struct DisplayOptionsObserver {
    _registration: Registration,
    _task: Task<()>,
}

impl Global for DisplayOptionsObserver {}

/// Keep open windows in step with accessibility changes. Call once at startup.
#[expect(
    unsafe_code,
    reason = "register a sendable notification block and read the AppKit notification name"
)]
pub(crate) fn install(cx: &mut App) {
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    let (wake_tx, mut wake_rx) = tokio::sync::mpsc::channel(1);
    let block = RcBlock::new(move |_: NonNull<NSNotification>| {
        // Notifications only wake the main-thread task. A pending wake already
        // covers later changes because the task reads the current OS values.
        let _ = wake_tx.try_send(());
    });
    // SAFETY: AppKit provides this notification name since macOS 10.10,
    // below the 13.0 deployment floor. The block captures only a Send sender;
    // it does not access UI state or the notification pointer. The center
    // copies the block, and Registration owns its returned observer token.
    let token = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification),
            None,
            None,
            &block,
        )
    };
    let registration = Registration { center, token };
    let mut allowed = allows_glass();
    let task = cx.spawn(async move |cx| {
        loop {
            // Reconcile periodically as well as on native notifications.
            // A missed notification must not leave an inaccessible background.
            let reconcile = cx.background_executor().timer(Duration::from_secs(30));
            tokio::select! {
                change = wake_rx.recv() => {
                    if change.is_none() {
                        break;
                    }
                }
                () = reconcile => {}
            }
            let current = allows_glass();
            if current != allowed {
                allowed = current;
                cx.update(|cx| crate::ui::theme::apply_from_settings(None, cx));
            }
        }
    });
    cx.set_global(DisplayOptionsObserver {
        _registration: registration,
        _task: task,
    });
}
