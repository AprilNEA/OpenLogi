//! Frontmost-application tracking on macOS: the `NSWorkspace` activation
//! observer, the conversion of an `NSRunningApplication` into a
//! [`ForegroundApp`], and the Safari process snapshot every such read refreshes.
//!
//! None of it runs on the tap thread, and all of it runs under an explicit
//! autorelease pool: its callers have no run loop to drain one.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicI32, Ordering};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_app_kit::{
    NSRunningApplication, NSWorkspace, NSWorkspaceApplicationKey,
    NSWorkspaceDidActivateApplicationNotification,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};
use tracing::error;

use crate::ForegroundApp;

/// Owner of an `NSWorkspace` activation observer.
///
/// The notification center retains the registration block. The returned token
/// identifies that registration; removing it releases the center's block
/// reference, and dropping the token releases the caller's final reference.
#[must_use]
pub struct ForegroundApplicationObserver {
    center: Retained<NSNotificationCenter>,
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

const SAFARI_BUNDLE_ID: &str = "com.apple.Safari";
const NO_SAFARI_PROCESS: i32 = 0;
static FRONTMOST_SAFARI_PID: AtomicI32 = AtomicI32::new(NO_SAFARI_PROCESS);

fn safari_process_id(bundle_id: &str, pid: i32) -> Option<i32> {
    (bundle_id == SAFARI_BUNDLE_ID && pid > 0).then_some(pid)
}

pub(super) fn observe_frontmost_application(
    app: Option<&NSRunningApplication>,
    pool: objc2::rc::AutoreleasePool<'_>,
) -> Option<ForegroundApp> {
    let foreground = app.and_then(|app| foreground_app_from_running_application(app, pool));
    let safari_pid = app
        .zip(foreground.as_ref())
        .and_then(|(app, foreground)| safari_process_id(&foreground.id, app.processIdentifier()))
        .unwrap_or(NO_SAFARI_PROCESS);
    FRONTMOST_SAFARI_PID.store(safari_pid, Ordering::Release);
    foreground
}

pub(crate) fn frontmost_safari_pid() -> Option<i32> {
    let pid = FRONTMOST_SAFARI_PID.load(Ordering::Acquire);
    (pid > 0).then_some(pid)
}

impl Drop for ForegroundApplicationObserver {
    fn drop(&mut self) {
        objc2::rc::autoreleasepool(|_| {
            // SAFETY: `token` came from this center's block-observer registration
            // and is removed exactly once, before both retained objects are dropped.
            unsafe { self.center.removeObserver(self.token.as_ref()) };
        });
    }
}

/// Register for `NSWorkspaceDidActivateApplicationNotification`.
pub(crate) fn watch_frontmost_application_activations(
    on_activation: impl Fn(Option<ForegroundApp>) + Send + Sync + 'static,
) -> ForegroundApplicationObserver {
    objc2::rc::autoreleasepool(|_| {
        let workspace = NSWorkspace::sharedWorkspace();
        let center = workspace.notificationCenter();
        let block: RcBlock<dyn Fn(NonNull<NSNotification>)> =
            RcBlock::new(move |notification: NonNull<NSNotification>| {
                // A panic must not unwind across the Objective-C block boundary.
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let activation = objc2::rc::autoreleasepool(|pool| {
                        // SAFETY: NotificationCenter passes a live, non-null
                        // NSNotification to the block for the duration of this call.
                        let notification = unsafe { notification.as_ref() };
                        let app = notification.userInfo().and_then(|info| {
                            // SAFETY: AppKit documents NSWorkspaceApplicationKey as this
                            // notification's NSRunningApplication-valued user-info entry.
                            info.objectForKey(unsafe { NSWorkspaceApplicationKey } as &AnyObject)?
                                .downcast::<NSRunningApplication>()
                                .ok()
                        });
                        observe_frontmost_application(app.as_deref(), pool)
                    });
                    on_activation(activation);
                }));
                if result.is_err() {
                    error!("foreground-application activation callback panicked");
                }
            });
        // SAFETY: AppKit exports the name as an immutable process-lifetime
        // constant. The block captures only `Send + Sync` state and accepts the
        // exact `NSNotification` argument required by the API. A nil queue asks
        // the center to invoke it synchronously on the notification-posting thread.
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceDidActivateApplicationNotification),
                Some(&workspace),
                None,
                &block,
            )
        };
        ForegroundApplicationObserver { center, token }
    })
}

pub(super) fn foreground_app_from_running_application(
    app: &NSRunningApplication,
    pool: objc2::rc::AutoreleasePool<'_>,
) -> Option<ForegroundApp> {
    // Unbundled applications still need an identifier for per-app profiles.
    let identifier = match app.bundleIdentifier() {
        Some(bundle_id) => bundle_id,
        None => app.executableURL().and_then(|url| url.path())?,
    };
    let name = app.localizedName();
    // SAFETY: Both UTF-8 views are copied into owned Strings before `pool`
    // drains, so no borrowed Objective-C storage escapes.
    let (id, name) = unsafe {
        (
            identifier.to_str(pool).to_owned(),
            name.as_ref().map(|name| name.to_str(pool).to_owned()),
        )
    };
    let display_name = name.unwrap_or_else(|| id.clone());
    Some(ForegroundApp { id, display_name })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a macOS desktop session with bundled and unbundled applications"]
    fn running_application_identity_prefers_bundle_id_then_executable_path() {
        let (unbundled, bundled) = objc2::rc::autoreleasepool(|pool| {
            let applications = NSWorkspace::sharedWorkspace().runningApplications();
            let (unbundled_app, executable_id) = applications
                .iter()
                .find_map(|app| {
                    if app.bundleIdentifier().is_some() || app.isTerminated() {
                        return None;
                    }
                    let path = app.executableURL()?.path()?.to_string();
                    (!path.is_empty()).then_some((app, path))
                })
                .expect("requires a running unbundled application with an executable path");
            let (bundled_app, bundle_id, executable_path) = applications
                .iter()
                .find_map(|app| {
                    if app.isTerminated() {
                        return None;
                    }
                    let bundle_id = app.bundleIdentifier()?.to_string();
                    let path = app.executableURL()?.path()?.to_string();
                    (!bundle_id.is_empty() && !path.is_empty()).then_some((app, bundle_id, path))
                })
                .expect("requires a running bundled application with an executable path");
            assert_ne!(bundle_id, executable_path);

            let unbundled_name = unbundled_app
                .localizedName()
                .map_or_else(|| executable_id.clone(), |name| name.to_string());
            let bundled_name = bundled_app
                .localizedName()
                .map_or_else(|| bundle_id.clone(), |name| name.to_string());
            eprintln!(
                "unbundled native fixture: pid={}, bundle_id=None, path={executable_id:?}",
                unbundled_app.processIdentifier()
            );
            eprintln!(
                "bundled native fixture: pid={}, bundle_id={bundle_id:?}, path={executable_path:?}",
                bundled_app.processIdentifier()
            );

            (
                (
                    foreground_app_from_running_application(&unbundled_app, pool),
                    ForegroundApp {
                        id: executable_id,
                        display_name: unbundled_name,
                    },
                ),
                (
                    foreground_app_from_running_application(&bundled_app, pool),
                    ForegroundApp {
                        id: bundle_id,
                        display_name: bundled_name,
                    },
                ),
            )
        });

        assert_eq!(bundled.0, Some(bundled.1));
        assert_eq!(unbundled.0, Some(unbundled.1));
    }

    #[test]
    fn safari_snapshot_accepts_only_safari_with_a_positive_pid() {
        assert_eq!(safari_process_id(SAFARI_BUNDLE_ID, 417), Some(417));
        assert_eq!(safari_process_id("com.apple.finder", 417), None);
        assert_eq!(safari_process_id(SAFARI_BUNDLE_ID, 0), None);
    }
}
