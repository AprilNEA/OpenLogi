//! Native Windows foreground-application activation observer.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, TryLockError, mpsc};
use std::thread;

use thiserror::Error;
use windows_sys::Win32::Foundation::{GetLastError, HWND};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EVENT_SYSTEM_FOREGROUND, MSG, PM_NOREMOVE, PeekMessageW, PostQuitMessage, PostThreadMessageW,
    WINEVENT_OUTOFCONTEXT, WM_APP, WM_QUIT, WM_USER,
};

use super::{Backend, MessageLoopExit, message_loop};
use crate::windows_worker::{
    ForegroundChanges, NotificationLatch, WorkerEvent, WorkerPhase, WorkerStatus,
};
use crate::{ForegroundApp, HookBackend};

/// Thread message asking the observer pump to take an authoritative foreground
/// snapshot. No `HWND` is carried: events queued before the initial snapshot
/// must not replay stale window identities after it.
const WM_FOREGROUND_CHANGED: u32 = WM_APP + 1;

type ActivationCallback = Box<dyn Fn(Option<ForegroundApp>) + Send + Sync + 'static>;

/// Typed setup and worker failures for the Windows foreground observer.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum ForegroundApplicationObserverError {
    /// The dedicated message-pump thread could not be spawned.
    #[error("could not spawn Windows foreground observer thread: {0}")]
    ThreadSpawn(String),
    /// The setup thread exited without reporting a setup result.
    #[error("Windows foreground observer thread exited during setup")]
    ThreadExitedDuringSetup,
    /// Only one process-global WinEvent callback owner can be installed.
    #[error("another Windows foreground observer is already installed")]
    AlreadyInstalled,
    /// The process-global callback ownership mutex was poisoned.
    #[error("Windows foreground observer callback ownership is poisoned")]
    CallbackOwnershipPoisoned,
    /// `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` failed.
    #[error("SetWinEventHook(EVENT_SYSTEM_FOREGROUND) failed with GetLastError={code}")]
    HookSetup { code: u32 },
    /// The WinEvent callback could not queue delivery on its owning thread.
    #[error("Windows foreground observer could not queue an event: GetLastError={code}")]
    EventDelivery { code: u32 },
    /// The owning thread's Win32 message loop failed.
    #[error("Windows foreground observer message loop failed with GetLastError={code}")]
    MessageLoop { code: u32 },
    /// The worker stopped without a typed native failure or a stop request.
    #[error("Windows foreground observer stopped unexpectedly")]
    WorkerStopped,
}

/// The non-blocking state the native WinEvent callback is allowed to touch.
struct CallbackTarget {
    thread_id: u32,
    pending: NotificationLatch,
    /// Zero means no failure. A set bit above the Win32 error-code range makes
    /// even GetLastError=0 representable as a recorded failure.
    delivery_failure: AtomicU64,
}

impl CallbackTarget {
    fn new(thread_id: u32) -> Self {
        Self {
            thread_id,
            pending: NotificationLatch::new(),
            delivery_failure: AtomicU64::new(0),
        }
    }

    fn queue_snapshot(&self) {
        if !self.pending.claim() {
            return;
        }
        // SAFETY: `thread_id` names the observer thread, whose message queue is
        // created before callback ownership is published; this message carries
        // no pointers and only asks that thread to take a fresh snapshot.
        if unsafe { PostThreadMessageW(self.thread_id, WM_FOREGROUND_CHANGED, 0, 0) } != 0 {
            return;
        }

        // SAFETY: GetLastError immediately follows the failed Win32 call and
        // has no preconditions.
        let code = unsafe { GetLastError() };
        let encoded = (1_u64 << 32) | u64::from(code);
        let _ =
            self.delivery_failure
                .compare_exchange(0, encoded, Ordering::AcqRel, Ordering::Acquire);
        self.pending.delivered();

        // Out-of-context WinEvents are delivered on the SetWinEventHook caller
        // thread. Ending that pump makes the failure terminal rather than
        // silently leaving per-app profiles stale.
        // SAFETY: this callback runs on the observer thread; PostQuitMessage
        // posts WM_QUIT to the calling thread and takes no pointers.
        unsafe { PostQuitMessage(0) };
    }

    fn delivered(&self) {
        self.pending.delivered();
    }

    fn delivery_failure(&self) -> Option<u32> {
        let encoded = self.delivery_failure.load(Ordering::Acquire);
        let bytes = encoded.to_le_bytes();
        (encoded != 0).then_some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}

static CALLBACK_TARGET: Mutex<Option<Arc<CallbackTarget>>> = Mutex::new(None);

/// RAII owner of a native `HWINEVENTHOOK` on its installing thread.
struct WinEventHook(HWINEVENTHOOK);

impl Drop for WinEventHook {
    fn drop(&mut self) {
        // SAFETY: this guard remains on the thread that installed the live
        // handle, and Drop runs exactly once for that owned handle.
        if unsafe { UnhookWinEvent(self.0) } == 0 {
            // SAFETY: GetLastError immediately follows the failed unhook.
            let code = unsafe { GetLastError() };
            tracing::warn!(code, "could not unhook Windows foreground observer");
        }
    }
}

/// Worker-owned registration. Callback ownership is always cleared before the
/// native hook is released, including unwinding and setup-channel failure.
struct Registration {
    target: Arc<CallbackTarget>,
    hook: Option<WinEventHook>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        clear_callback_target(&self.target);
        drop(self.hook.take());
    }
}

struct Ready {
    thread_id: u32,
    target: Arc<CallbackTarget>,
}

/// RAII owner of the independent Windows foreground-observer worker.
///
/// Dropping it first prevents further callback queueing, then posts `WM_QUIT`
/// and synchronously joins the owning thread. The thread unhooks its WinEvent
/// registration before the join completes.
#[must_use]
pub(crate) struct ForegroundApplicationObserver {
    thread_id: u32,
    join: Option<thread::JoinHandle<()>>,
    worker: Arc<WorkerStatus>,
    failure: Arc<Mutex<Option<ForegroundApplicationObserverError>>>,
    target: Arc<CallbackTarget>,
}

impl ForegroundApplicationObserver {
    /// Return a typed failure if the worker can no longer deliver changes.
    pub(crate) fn check_health(&self) -> Result<(), ForegroundApplicationObserverError> {
        if let Some(code) = self.target.delivery_failure() {
            return Err(ForegroundApplicationObserverError::EventDelivery { code });
        }
        if let Some(error) = self
            .failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            return Err(error);
        }
        if self.worker.phase().is_running() {
            Ok(())
        } else {
            Err(ForegroundApplicationObserverError::WorkerStopped)
        }
    }
}

impl Drop for ForegroundApplicationObserver {
    fn drop(&mut self) {
        clear_callback_target(&self.target);
        let previous = self.worker.transition(WorkerEvent::StopRequested);
        if previous == WorkerPhase::Running {
            // SAFETY: `thread_id` came from the observer thread after it created
            // its queue. The message carries no pointers.
            if unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) } == 0 {
                // SAFETY: GetLastError immediately follows the failed post.
                let code = unsafe { GetLastError() };
                tracing::warn!(
                    code,
                    "could not post WM_QUIT to Windows foreground observer"
                );
            }
        }
        if let Some(join) = self.join.take()
            && let Err(error) = join.join()
        {
            tracing::warn!(
                ?error,
                "Windows foreground observer panicked while stopping"
            );
        }
    }
}

/// Subscribe to native foreground changes and publish an initial snapshot.
///
/// Registration happens before the initial authoritative read. Native bursts
/// are coalesced into a fresh [`Backend::frontmost_app`] read on the owning
/// message-pump thread, so startup events cannot replay a stale HWND after the
/// initial snapshot. `on_activation` runs on that worker and must return
/// quickly.
pub(crate) fn watch_frontmost_application_activations(
    on_activation: impl Fn(Option<ForegroundApp>) + Send + Sync + 'static,
) -> Result<ForegroundApplicationObserver, ForegroundApplicationObserverError> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let worker = Arc::new(WorkerStatus::new());
    let failure = Arc::new(Mutex::new(None));
    let thread_worker = Arc::clone(&worker);
    let thread_failure = Arc::clone(&failure);
    let join = thread::Builder::new()
        .name("openlogi-windows-foreground".into())
        .spawn(move || {
            let callback: ActivationCallback = Box::new(on_activation);
            observer_thread(&callback, &ready_tx, &thread_worker, &thread_failure);
        })
        .map_err(|error| ForegroundApplicationObserverError::ThreadSpawn(error.to_string()))?;

    match ready_rx.recv() {
        Ok(Ok(ready)) => Ok(ForegroundApplicationObserver {
            thread_id: ready.thread_id,
            join: Some(join),
            worker,
            failure,
            target: ready.target,
        }),
        Ok(Err(error)) => {
            let _ = join.join();
            Err(error)
        }
        Err(_) => {
            let _ = join.join();
            Err(ForegroundApplicationObserverError::ThreadExitedDuringSetup)
        }
    }
}

fn observer_thread(
    callback: &ActivationCallback,
    ready: &mpsc::Sender<Result<Ready, ForegroundApplicationObserverError>>,
    worker: &WorkerStatus,
    failure: &Mutex<Option<ForegroundApplicationObserverError>>,
) {
    // SAFETY: GetCurrentThreadId has no preconditions.
    let thread_id = unsafe { GetCurrentThreadId() };
    let mut bootstrap_msg = MSG::default();
    // SAFETY: this live MSG and null HWND inspect this thread's queue. The call
    // creates the queue before callback ownership becomes visible to WinEvent.
    unsafe {
        PeekMessageW(
            &raw mut bootstrap_msg,
            std::ptr::null_mut(),
            WM_USER,
            WM_USER,
            PM_NOREMOVE,
        );
    }

    let target = Arc::new(CallbackTarget::new(thread_id));
    if let Err(error) = claim_callback_target(&target) {
        let _ = ready.send(Err(error));
        return;
    }

    // SAFETY: `foreground_event_proc` has the exact WINEVENTPROC ABI. A null
    // module with WINEVENT_OUTOFCONTEXT is documented, and zero process/thread
    // ids subscribe to this event from every process on the current desktop.
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            std::ptr::null_mut(),
            Some(foreground_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    if hook.is_null() {
        // Read last-error before callback cleanup can call another Win32 API.
        // SAFETY: GetLastError immediately follows the failed setup call.
        let code = unsafe { GetLastError() };
        clear_callback_target(&target);
        let _ = ready.send(Err(ForegroundApplicationObserverError::HookSetup { code }));
        return;
    }
    let registration = Registration {
        target: Arc::clone(&target),
        hook: Some(WinEventHook(hook)),
    };

    worker.transition(WorkerEvent::Started);
    let mut changes = ForegroundChanges::default();
    publish_current(callback, &mut changes);
    if ready
        .send(Ok(Ready {
            thread_id,
            target: Arc::clone(&target),
        }))
        .is_err()
    {
        worker.transition(WorkerEvent::StopRequested);
        worker.transition(WorkerEvent::MessageLoopQuit);
        drop(registration);
        return;
    }

    let exit = message_loop(|msg| {
        if msg.message != WM_FOREGROUND_CHANGED {
            return false;
        }
        target.delivered();
        publish_current(callback, &mut changes);
        true
    });

    let error = match exit {
        MessageLoopExit::Quit => target
            .delivery_failure()
            .map(|code| ForegroundApplicationObserverError::EventDelivery { code }),
        MessageLoopExit::Failed(code) => {
            Some(ForegroundApplicationObserverError::MessageLoop { code })
        }
    };
    if let Some(error) = error {
        *failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(error.clone());
        worker.transition(WorkerEvent::MessageLoopFailed);
        tracing::error!(%error, "Windows foreground observer stopped");
    } else {
        worker.transition(WorkerEvent::MessageLoopQuit);
    }
    drop(registration);
}

fn publish_current(callback: &ActivationCallback, changes: &mut ForegroundChanges) {
    let current = Backend::frontmost_app();
    if !changes.observe(current.as_ref()) {
        return;
    }
    if catch_unwind(AssertUnwindSafe(|| callback(current))).is_err() {
        tracing::error!("Windows foreground-application callback panicked");
    }
}

fn claim_callback_target(
    target: &Arc<CallbackTarget>,
) -> Result<(), ForegroundApplicationObserverError> {
    let mut slot = CALLBACK_TARGET
        .lock()
        .map_err(|_| ForegroundApplicationObserverError::CallbackOwnershipPoisoned)?;
    if slot.is_some() {
        return Err(ForegroundApplicationObserverError::AlreadyInstalled);
    }
    *slot = Some(Arc::clone(target));
    Ok(())
}

fn clear_callback_target(target: &Arc<CallbackTarget>) {
    let mut slot = CALLBACK_TARGET
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if slot
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, target))
    {
        *slot = None;
    }
}

/// Win32 callback: coalesce and enqueue only. Process lookup, deduplication,
/// and user delivery stay outside this native boundary on the owning pump.
unsafe extern "system" fn foreground_event_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _object_id: i32,
    _child_id: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    // No Rust panic may unwind into user32. try_lock keeps teardown contention
    // from blocking the native callback; contention only occurs while callback
    // ownership is being installed or cleared.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let target = match CALLBACK_TARGET.try_lock() {
            Ok(slot) => slot.clone(),
            Err(TryLockError::WouldBlock | TryLockError::Poisoned(_)) => None,
        };
        if let Some(target) = target {
            target.queue_snapshot();
        }
    }));
}
