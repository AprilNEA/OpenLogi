//! Windows `WH_MOUSE_LL` implementation of the OS-level mouse hook.
#![allow(
    clippy::borrow_as_ptr,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::needless_pass_by_value,
    reason = "Win32 FFI uses raw pointer parameters and fixed-width message values"
)]

use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Threading::{
    GetCurrentThreadId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId,
    HC_ACTION, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_MOUSE_LL, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEWHEEL, WM_QUIT,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_USER, WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1, XBUTTON2,
};

use crate::{ButtonId, EventDisposition, HookError, MouseEvent};

const WHEEL_DELTA: f32 = 120.0;

type HookCallback = Arc<dyn Fn(MouseEvent) -> EventDisposition + Send + Sync + 'static>;

static CALLBACK: Mutex<Option<HookCallback>> = Mutex::new(None);

pub(crate) struct HookInner {
    thread_id: u32,
    join: Option<thread::JoinHandle<()>>,
}

pub(crate) fn start(
    cb: impl Fn(MouseEvent) -> EventDisposition + Send + Sync + 'static,
) -> Result<HookInner, HookError> {
    let callback: HookCallback = Arc::new(cb);
    let (ready_tx, ready_rx) = mpsc::channel();
    let join = thread::Builder::new()
        .name("openlogi-windows-hook".into())
        .spawn(move || hook_thread(callback, ready_tx))
        .map_err(|e| HookError::WindowsHook(format!("could not spawn hook thread: {e}")))?;

    match ready_rx
        .recv()
        .map_err(|e| HookError::WindowsHook(format!("hook thread exited before setup: {e}")))?
    {
        Ok(thread_id) => Ok(HookInner {
            thread_id,
            join: Some(join),
        }),
        Err(e) => {
            let _ = join.join();
            Err(e)
        }
    }
}

pub(crate) fn stop(mut inner: HookInner) {
    // SAFETY: PostThreadMessageW takes the target thread id and the message by
    // value (no pointers); `thread_id` was returned by the hook thread's own
    // GetCurrentThreadId, so it names a real thread with a message queue.
    let posted = unsafe { PostThreadMessageW(inner.thread_id, WM_QUIT, 0, 0) };
    if posted == 0 {
        // SAFETY: GetLastError reads the calling thread's last-error code and
        // has no preconditions.
        let err = unsafe { GetLastError() };
        tracing::warn!(error = err, "could not post WM_QUIT to Windows hook thread");
    }
    if let Some(join) = inner.join.take()
        && let Err(e) = join.join()
    {
        tracing::warn!(?e, "Windows hook thread panicked while stopping");
    }
}

fn hook_thread(callback: HookCallback, ready: mpsc::Sender<Result<u32, HookError>>) {
    match CALLBACK.lock() {
        Ok(mut slot) if slot.is_none() => {
            *slot = Some(callback);
        }
        Ok(_) => {
            let _ = ready.send(Err(HookError::WindowsHook(
                "another Windows mouse hook is already installed".into(),
            )));
            return;
        }
        Err(e) => {
            let _ = ready.send(Err(HookError::WindowsHook(format!(
                "callback lock poisoned: {e}"
            ))));
            return;
        }
    }

    // SAFETY: GetCurrentThreadId returns the calling thread's id; no preconditions.
    let thread_id = unsafe { GetCurrentThreadId() };
    let mut bootstrap_msg = MSG::default();
    // SAFETY: `bootstrap_msg` is a live, owned MSG and a null window handle is
    // valid (peek this thread's own queue); PM_NOREMOVE only inspects. The call
    // forces the OS to create this thread's message queue up front, so a
    // PostThreadMessageW from `stop` can't race queue creation and be lost.
    unsafe {
        PeekMessageW(
            &mut bootstrap_msg,
            std::ptr::null_mut(),
            WM_USER,
            WM_USER,
            PM_NOREMOVE,
        );
    }

    // SAFETY: `mouse_proc` is a valid HOOKPROC with the matching `extern "system"`
    // signature; a null module handle plus thread id 0 install a global
    // low-level mouse hook, the documented usage for WH_MOUSE_LL. Returns null
    // on failure, checked below.
    let hook = unsafe {
        SetWindowsHookExW(
            WH_MOUSE_LL,
            Some(mouse_proc),
            std::ptr::null_mut::<core::ffi::c_void>(),
            0,
        )
    };
    if hook.is_null() {
        clear_callback();
        let _ = ready.send(Err(last_error("SetWindowsHookExW")));
        return;
    }

    let _ = ready.send(Ok(thread_id));
    message_loop();

    // SAFETY: `hook` is the live handle just returned by SetWindowsHookExW,
    // unhooked exactly once here as the thread exits.
    unsafe {
        UnhookWindowsHookEx(hook);
    }
    clear_callback();
}

fn message_loop() {
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a live, owned MSG; a null window handle retrieves
        // messages for the calling thread. Returns <= 0 on WM_QUIT or error.
        let result = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if result <= 0 {
            break;
        }
        // SAFETY: `msg` was just populated by GetMessageW and outlives the call.
        unsafe { TranslateMessage(&msg) };
        // SAFETY: as above — `msg` is a live, initialized MSG.
        unsafe { DispatchMessageW(&msg) };
    }
}

fn clear_callback() {
    if let Ok(mut slot) = CALLBACK.lock() {
        *slot = None;
    }
}

/// Forward the event to the next hook in the chain — the default disposition
/// for any event we don't suppress.
fn call_next(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: a null `hhk` is the documented way to invoke the next hook in the
    // chain; `code`/`wparam`/`lparam` are forwarded verbatim from the
    // OS-supplied callback arguments, valid for the duration of this call.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Low-level mouse-hook procedure the OS invokes for every mouse event.
///
/// # Safety
/// Must only be installed as a `WH_MOUSE_LL` hook via `SetWindowsHookExW`. When
/// `code == HC_ACTION`, Windows guarantees `lparam` points to a live
/// `MSLLHOOKSTRUCT`; [`hook_data`] relies on that contract to dereference it.
unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code != HC_ACTION as i32 {
        return call_next(code, wparam, lparam);
    }

    // SAFETY: `mouse_proc` is only installed as a WH_MOUSE_LL hook and this is
    // the `code == HC_ACTION` arm, so `lparam` is the live `MSLLHOOKSTRUCT`
    // pointer `hook_data` requires.
    let Some(data) = (unsafe { hook_data(lparam) }) else {
        return call_next(code, wparam, lparam);
    };
    let Some(event) = translate_event(wparam, data) else {
        return call_next(code, wparam, lparam);
    };

    let callback = CALLBACK.lock().ok().and_then(|slot| slot.clone());
    let disposition = callback
        .as_ref()
        .map_or(EventDisposition::PassThrough, |cb| cb(event));
    if disposition == EventDisposition::Suppress {
        1
    } else {
        call_next(code, wparam, lparam)
    }
}

/// Copy the `MSLLHOOKSTRUCT` the OS passed in `lparam`, or `None` if `lparam`
/// is null.
///
/// # Safety
/// `lparam` must be the `lParam` the OS passes to a `WH_MOUSE_LL` hook
/// procedure for an `HC_ACTION` event — i.e. it points to a live
/// `MSLLHOOKSTRUCT` (or is 0). Any other non-zero value is undefined behavior.
unsafe fn hook_data(lparam: LPARAM) -> Option<MSLLHOOKSTRUCT> {
    if lparam == 0 {
        return None;
    }
    // SAFETY: by this function's contract `lparam` is the WH_MOUSE_LL HC_ACTION
    // lParam and is non-zero here, so it points to a live `MSLLHOOKSTRUCT`. We
    // copy it out by value (plain-old-data) and never retain the pointer.
    Some(unsafe { *(lparam as *const MSLLHOOKSTRUCT) })
}

fn translate_event(wparam: WPARAM, data: MSLLHOOKSTRUCT) -> Option<MouseEvent> {
    if data.flags & LLMHF_INJECTED != 0 {
        return None;
    }

    let pressed = match wparam as u32 {
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN => Some(true),
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP | WM_XBUTTONUP => Some(false),
        _ => None,
    };
    if let Some(pressed) = pressed {
        let id = match wparam as u32 {
            WM_LBUTTONDOWN | WM_LBUTTONUP => ButtonId::LeftClick,
            WM_RBUTTONDOWN | WM_RBUTTONUP => ButtonId::RightClick,
            WM_MBUTTONDOWN | WM_MBUTTONUP => ButtonId::MiddleClick,
            WM_XBUTTONDOWN | WM_XBUTTONUP => match high_word(data.mouseData) {
                XBUTTON1 => ButtonId::Back,
                XBUTTON2 => ButtonId::Forward,
                _ => return None,
            },
            _ => return None,
        };
        return Some(MouseEvent::Button { id, pressed });
    }

    match wparam as u32 {
        // A positive high word means the wheel was rotated forward (away from the
        // user). Pass the sign through unchanged so `delta_y > 0` is "scroll up" on
        // every platform — matching macOS (`SCROLL_WHEEL_EVENT_DELTA_AXIS_1`) and
        // Linux (`REL_WHEEL`), whose deltas feed the same direction-sensitive
        // bindings. Negating here flipped scroll-up/-down only on Windows.
        WM_MOUSEWHEEL => Some(MouseEvent::Scroll {
            delta_x: 0.0,
            delta_y: f32::from(signed_high_word(data.mouseData)) / WHEEL_DELTA,
        }),
        WM_MOUSEHWHEEL => Some(MouseEvent::Scroll {
            delta_x: f32::from(signed_high_word(data.mouseData)) / WHEEL_DELTA,
            delta_y: 0.0,
        }),
        _ => None,
    }
}

fn high_word(value: u32) -> u16 {
    (value >> 16) as u16
}

fn signed_high_word(value: u32) -> i16 {
    high_word(value) as i16
}

pub(crate) fn frontmost_process_path() -> Option<String> {
    // SAFETY: GetForegroundWindow takes no arguments and returns a window handle
    // or null; no preconditions.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return None;
    }

    let mut pid = 0;
    // SAFETY: `hwnd` is the non-null handle just returned; `&mut pid` is a valid
    // out-pointer the call writes the owning process id into.
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
    }
    if pid == 0 {
        return None;
    }

    // SAFETY: OpenProcess takes the access mask and pid by value and returns a
    // handle or null (checked); on success we own the handle and close it below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }

    let mut buf = vec![0u16; 32_768];
    let mut len = buf.len() as u32;
    // SAFETY: `process` is the valid handle from OpenProcess; `buf` is a live
    // 32768-u16 buffer and `len` holds its length, so the call writes at most
    // `len` code units and updates `len` with the count written.
    let ok = unsafe { QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut len) };
    // SAFETY: `process` is the handle from OpenProcess, owned here and closed
    // exactly once now that the query has returned.
    unsafe {
        CloseHandle(process);
    }
    if ok == 0 || len == 0 {
        return None;
    }

    Some(String::from_utf16_lossy(&buf[..len as usize]).to_lowercase())
}

fn last_error(context: &str) -> HookError {
    // SAFETY: GetLastError reads the calling thread's last-error code; no preconditions.
    let code = unsafe { GetLastError() };
    HookError::WindowsHook(format!("{context} failed with GetLastError={code}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_event_ignores_injected_mouse_input() {
        let data = MSLLHOOKSTRUCT {
            flags: LLMHF_INJECTED,
            ..MSLLHOOKSTRUCT::default()
        };

        assert!(translate_event(WM_LBUTTONDOWN as WPARAM, data).is_none());
    }

    /// Wheel-forward (away from the user) must produce a positive `delta_y`, the
    /// same sign macOS and Linux emit for the gesture, so a "scroll up" binding
    /// fires on the same physical motion on every platform. Guards against the
    /// sign inversion that previously flipped scroll direction on Windows.
    #[test]
    fn wheel_forward_scrolls_up_like_other_platforms() {
        // The wheel delta lives in the high word of `mouseData`; `+WHEEL_DELTA`
        // (120) is one notch forward.
        let forward = MSLLHOOKSTRUCT {
            mouseData: 120u32 << 16,
            ..MSLLHOOKSTRUCT::default()
        };
        let Some(MouseEvent::Scroll { delta_x, delta_y }) =
            translate_event(WM_MOUSEWHEEL as WPARAM, forward)
        else {
            panic!("expected a scroll event");
        };
        assert!(delta_x.abs() < f32::EPSILON);
        assert!(
            delta_y > 0.0,
            "wheel-forward should scroll up, got {delta_y}"
        );
    }
}
