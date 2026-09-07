//! Agent-owned macOS permission prompts.
//!
//! The native sheets must name this process (`org.openlogi.agent`), so these
//! helpers run here rather than in the GUI. Arming never calls them — a login
//! start would suppress or poison the sheet. The GUI asks over IPC after its
//! first Ready snapshot.

#![expect(
    unsafe_code,
    reason = "CoreBluetooth force-link + `CBCentralManager` alloc/init and authorization send"
)]

use std::cell::RefCell;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use dispatch2::DispatchQueue;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use tracing::warn;

use crate::binary_watch;

/// How long to wait for the user to answer the Bluetooth consent sheet.
const BLUETOOTH_PROMPT_TIMEOUT: Duration = Duration::from_secs(180);
const BLUETOOTH_POLL: Duration = Duration::from_millis(200);

// Force-link CoreBluetooth so `CBCentralManager` is registered for the lookup
// and for `alloc`/`initWithDelegate:queue:`.
#[link(name = "CoreBluetooth", kind = "framework")]
unsafe extern "C" {}

thread_local! {
    /// `CBCentralManager` must be created and dropped on the AppKit thread.
    /// Held only while [`request_bluetooth`] is polling authorization.
    static BLUETOOTH_MANAGER: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
}

/// One in-flight Bluetooth sheet at a time. Overlapping auto + Settings
/// Grant requests share the main-thread manager slot; without this lock the
/// second call would replace the first manager and either cleanup could
/// drop the other while it is still polling.
static BLUETOOTH_PROMPT: Mutex<()> = Mutex::new(());

/// Request Input Monitoring so HID inventory can succeed.
///
/// A newly granted permission requires a process relaunch before macOS lets
/// the agent open HID devices. Trust [`openlogi_hid::permissions::request_access`]'s
/// return, not a follow-up `IOHIDCheckAccess` — that check stays stale in the
/// granting process.
pub async fn request_input_monitoring() -> bool {
    if openlogi_hid::permissions::has_access() {
        return true;
    }
    let granted = match tokio::task::spawn_blocking(openlogi_hid::permissions::request_access).await
    {
        Ok(granted) => granted,
        Err(e) => {
            warn!(error = %e, "Input Monitoring permission request task failed");
            return false;
        }
    };
    if granted {
        binary_watch::relaunch_after_input_monitoring_grant();
    }
    granted
}

/// Request CoreBluetooth authorization so the native sheet names this helper.
///
/// `CBCentralManager` is created on the main queue (AppKit). Authorization is
/// polled off the main thread so the tray run loop stays alive while the user
/// answers.
pub async fn request_bluetooth() -> bool {
    match tokio::task::spawn_blocking(request_bluetooth_access).await {
        Ok(granted) => granted,
        Err(e) => {
            warn!(error = %e, "Bluetooth permission request task failed");
            false
        }
    }
}

fn bluetooth_authorization() -> isize {
    let Some(cls) = AnyClass::get(c"CBCentralManager") else {
        return 0;
    };
    // SAFETY: `+[CBManager authorization]` is a documented class method
    // returning a `CBManagerAuthorization` NSInteger.
    unsafe { msg_send![cls, authorization] }
}

fn create_central_manager() -> Option<Retained<AnyObject>> {
    let cls = AnyClass::get(c"CBCentralManager")?;
    let nil: *mut AnyObject = std::ptr::null_mut();
    // SAFETY: `+[CBCentralManager alloc]` / `-initWithDelegate:queue:` are
    // the documented constructors; a nil delegate and nil queue are accepted.
    unsafe {
        let alloc: *mut AnyObject = msg_send![cls, alloc];
        if alloc.is_null() {
            return None;
        }
        let init: *mut AnyObject = msg_send![alloc, initWithDelegate: nil, queue: nil];
        Retained::from_raw(init)
    }
}

fn request_bluetooth_access() -> bool {
    let _guard = BLUETOOTH_PROMPT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match bluetooth_authorization() {
        3 => return true,
        1 | 2 => return false,
        _ => {}
    }

    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    DispatchQueue::main().exec_sync(move || {
        let manager = create_central_manager();
        let created = manager.is_some();
        BLUETOOTH_MANAGER.with(|slot| {
            *slot.borrow_mut() = manager;
        });
        let _ = tx.send(created);
    });
    if rx.recv() != Ok(true) {
        return false;
    }

    let deadline = Instant::now() + BLUETOOTH_PROMPT_TIMEOUT;
    let granted = loop {
        match bluetooth_authorization() {
            3 => break true,
            1 | 2 => break false,
            _ if Instant::now() >= deadline => break false,
            _ => thread::sleep(BLUETOOTH_POLL),
        }
    };

    DispatchQueue::main().exec_sync(|| {
        BLUETOOTH_MANAGER.with(|slot| {
            slot.borrow_mut().take();
        });
    });
    granted
}

#[cfg(test)]
mod tests {
    #[test]
    fn bluetooth_authorization_is_a_query_only() {
        let _ = super::bluetooth_authorization();
    }
}
