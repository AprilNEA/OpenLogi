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
    let posted = unsafe { PostThreadMessageW(inner.thread_id, WM_QUIT, 0, 0) };
    if posted == 0 {
        tracing::warn!(
            error = unsafe { GetLastError() },
            "could not post WM_QUIT to Windows hook thread"
        );
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

    let thread_id = unsafe { GetCurrentThreadId() };
    let mut bootstrap_msg = MSG::default();
    unsafe {
        PeekMessageW(
            &mut bootstrap_msg,
            std::ptr::null_mut(),
            WM_USER,
            WM_USER,
            PM_NOREMOVE,
        );
    }

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

    unsafe {
        UnhookWindowsHookEx(hook);
    }
    clear_callback();
}

fn message_loop() {
    let mut msg = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if result <= 0 {
            break;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn clear_callback() {
    if let Ok(mut slot) = CALLBACK.lock() {
        *slot = None;
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code != HC_ACTION as i32 {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    }

    let Some(data) = hook_data(lparam) else {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    };
    let Some(event) = translate_event(wparam, data) else {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    };

    let callback = CALLBACK.lock().ok().and_then(|slot| slot.clone());
    let disposition = callback
        .as_ref()
        .map_or(EventDisposition::PassThrough, |cb| cb(event));
    if disposition == EventDisposition::Suppress {
        1
    } else {
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }
}

fn hook_data(lparam: LPARAM) -> Option<MSLLHOOKSTRUCT> {
    if lparam == 0 {
        return None;
    }
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
            is_continuous: false,
        }),
        WM_MOUSEHWHEEL => Some(MouseEvent::Scroll {
            delta_x: f32::from(signed_high_word(data.mouseData)) / WHEEL_DELTA,
            delta_y: 0.0,
            is_continuous: false,
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
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return None;
    }

    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
    }
    if pid == 0 {
        return None;
    }

    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }

    let mut buf = vec![0u16; 32_768];
    let mut len = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut len) };
    unsafe {
        CloseHandle(process);
    }
    if ok == 0 || len == 0 {
        return None;
    }

    Some(String::from_utf16_lossy(&buf[..len as usize]).to_lowercase())
}

fn last_error(context: &str) -> HookError {
    HookError::WindowsHook(format!("{context} failed with GetLastError={}", unsafe {
        GetLastError()
    }))
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
        let Some(MouseEvent::Scroll {
            delta_x, delta_y, ..
        }) = translate_event(WM_MOUSEWHEEL as WPARAM, forward)
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
