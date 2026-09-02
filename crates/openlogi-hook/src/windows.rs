//! Windows `WH_MOUSE_LL` + `WH_KEYBOARD_LL` implementation of the OS-level
//! input hook.
#![expect(
    unsafe_code,
    reason = "the low-level input hook is built on the Win32 C API"
)]

use std::cell::Cell;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromPoint};
use windows_sys::Win32::System::Threading::{
    GetCurrentThreadId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_ESCAPE, VK_F1, VK_LWIN, VK_MENU, VK_RWIN,
    VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetCursorPos, GetForegroundWindow, GetMessageW,
    GetWindowThreadProcessId, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLMHF_INJECTED, MSG,
    MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, USER_DEFAULT_SCREEN_DPI, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_USER, WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1, XBUTTON2,
};

use crate::windows_worker::{WorkerEvent, WorkerPhase, WorkerStatus};
use crate::{
    ButtonId, CursorPosition, EventDisposition, ForegroundApp, HookBackend, HookError, HookEvent,
    KeyEvent, KeyModifiers, MouseEvent, ScrollDelta,
};

pub(crate) mod foreground;

const WHEEL_DELTA: f64 = 120.0;

thread_local! {
    /// Cursor position carried by the previous mouse message of any kind,
    /// differenced into the relative delta [`MouseEvent::Moved`] carries — see
    /// [`translate_event`] and [`motion_delta`].
    ///
    /// Thread-local rather than a `static`: `WH_MOUSE_LL` callbacks run on the
    /// thread that installed the hook, so this is single-threaded by
    /// construction and needs no synchronization on the freeze-sensitive
    /// callback path.
    static LAST_POINT: Cell<Option<POINT>> = const { Cell::new(None) };
}

type HookCallback = Arc<dyn Fn(HookEvent) -> EventDisposition + Send + Sync + 'static>;

static CALLBACK: Mutex<Option<HookCallback>> = Mutex::new(None);

pub(crate) struct HookInner {
    thread_id: u32,
    join: Option<thread::JoinHandle<()>>,
    worker: Arc<WorkerStatus>,
}

/// The Windows backend: `WH_MOUSE_LL` / `WH_KEYBOARD_LL` hooks on a thread
/// with its own message pump.
pub(crate) struct Backend;

impl HookBackend for Backend {
    type Running = HookInner;

    fn start(
        cb: impl Fn(HookEvent) -> EventDisposition + Send + Sync + 'static,
    ) -> Result<HookInner, HookError> {
        let callback: HookCallback = Arc::new(cb);
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker = Arc::new(WorkerStatus::new());
        let thread_worker = Arc::clone(&worker);
        let join = thread::Builder::new()
            .name("openlogi-windows-hook".into())
            .spawn(move || hook_thread(callback, ready_tx, &thread_worker))
            .map_err(|e| HookError::WindowsHook(format!("could not spawn hook thread: {e}")))?;

        match ready_rx
            .recv()
            .map_err(|e| HookError::WindowsHook(format!("hook thread exited before setup: {e}")))?
        {
            Ok(thread_id) => Ok(HookInner {
                thread_id,
                join: Some(join),
                worker,
            }),
            Err(e) => {
                let _ = join.join();
                Err(e)
            }
        }
    }

    fn stop(mut inner: HookInner) {
        let previous = inner.worker.transition(WorkerEvent::StopRequested);
        if previous == WorkerPhase::Running {
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
        }
        if let Some(join) = inner.join.take()
            && let Err(e) = join.join()
        {
            tracing::warn!(?e, "Windows hook thread panicked while stopping");
        }
    }

    fn is_running(inner: &HookInner) -> bool {
        inner.worker.phase().is_running()
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "the path buffer is a fixed 32768 u16s"
    )]
    fn frontmost_app() -> Option<ForegroundApp> {
        // SAFETY: GetForegroundWindow takes no arguments and returns a window handle
        // or null; no preconditions.
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.is_null() {
            return None;
        }

        let mut pid = 0;
        // SAFETY: `hwnd` is the non-null handle just returned; `&raw mut pid` is a
        // valid out-pointer the call writes the owning process id into.
        unsafe {
            GetWindowThreadProcessId(hwnd, &raw mut pid);
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
        let ok = unsafe { QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &raw mut len) };
        // SAFETY: `process` is the handle from OpenProcess, owned here and closed
        // exactly once now that the query has returned.
        unsafe {
            CloseHandle(process);
        }
        if ok == 0 || len == 0 {
            return None;
        }

        // The lower-cased full path is the identifier profiles key on (it is
        // what `Config::effective_bindings` compares, alongside its
        // `exe:<filename>` fallback); the file name is all there is to show a
        // human, so the display name is the stem with its original casing.
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let display_name = std::path::Path::new(&path)
            .file_stem()
            .map_or_else(|| path.clone(), |stem| stem.to_string_lossy().into_owned());
        Some(ForegroundApp {
            id: path.to_lowercase(),
            display_name,
        })
    }

    fn cursor_position() -> Option<CursorPosition> {
        let mut point = POINT { x: 0, y: 0 };
        // SAFETY: `point` is a valid writable POINT for the duration of the call.
        if unsafe { GetCursorPos(&raw mut point) } == 0 {
            return None;
        }
        // `GetCursorPos` reports physical pixels, but every consumer of
        // `CursorPosition` (currently the overlay, matching cursor against
        // GPUI's own display bounds) works in DIPs — GPUI's Windows backend
        // divides every display and window bounds by the monitor's DPI scale.
        // Left un-normalized, the ring drifts away from the cursor by exactly
        // the scale factor on any monitor above 100% scaling.
        // SAFETY: `MonitorFromPoint` takes `point` by value and never fails —
        // `MONITOR_DEFAULTTONEAREST` guarantees a valid `HMONITOR` even when
        // `point` sits outside every monitor.
        let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
        let mut dpi_x = 0u32;
        let mut dpi_y = 0u32;
        // SAFETY: `monitor` is a valid `HMONITOR` from `MonitorFromPoint` above;
        // `dpi_x`/`dpi_y` are valid writable `u32`s for the duration of the call.
        // The HRESULT is checked below rather than trusted, since MSDN leaves
        // the out-params unspecified (not necessarily untouched) on failure.
        let hr =
            unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut dpi_x, &raw mut dpi_y) };
        let (dpi_x, dpi_y) = if hr >= 0 && dpi_x > 0 && dpi_y > 0 {
            (dpi_x, dpi_y)
        } else {
            (USER_DEFAULT_SCREEN_DPI, USER_DEFAULT_SCREEN_DPI)
        };
        let scale_x = f64::from(dpi_x) / f64::from(USER_DEFAULT_SCREEN_DPI);
        let scale_y = f64::from(dpi_y) / f64::from(USER_DEFAULT_SCREEN_DPI);
        Some(CursorPosition {
            x: f64::from(point.x) / scale_x,
            y: f64::from(point.y) / scale_y,
        })
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "callback and ready are moved into the thread's hook state and channel"
)]
fn hook_thread(
    callback: HookCallback,
    ready: mpsc::Sender<Result<u32, HookError>>,
    worker: &WorkerStatus,
) {
    match CALLBACK.lock() {
        Ok(mut slot) if slot.is_none() => {
            *slot = Some(callback);
        }
        Ok(_) => {
            let _ = ready.send(Err(HookError::WindowsHook(
                "another Windows input hook is already installed".into(),
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
            &raw mut bootstrap_msg,
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
    let mouse_hook = unsafe {
        SetWindowsHookExW(
            WH_MOUSE_LL,
            Some(mouse_proc),
            std::ptr::null_mut::<core::ffi::c_void>(),
            0,
        )
    };
    if mouse_hook.is_null() {
        clear_callback();
        let _ = ready.send(Err(last_error("SetWindowsHookExW(WH_MOUSE_LL)")));
        return;
    }

    // SAFETY: same contract as the mouse hook above, with `keyboard_proc` as
    // the HOOKPROC — the documented usage for WH_KEYBOARD_LL.
    let keyboard_hook = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(keyboard_proc),
            std::ptr::null_mut::<core::ffi::c_void>(),
            0,
        )
    };
    if keyboard_hook.is_null() {
        // Read the error before UnhookWindowsHookEx can overwrite it.
        let error = last_error("SetWindowsHookExW(WH_KEYBOARD_LL)");
        // SAFETY: `mouse_hook` is the live handle just returned above, unhooked
        // exactly once on this bail-out path.
        unsafe {
            UnhookWindowsHookEx(mouse_hook);
        }
        clear_callback();
        let _ = ready.send(Err(error));
        return;
    }

    worker.transition(WorkerEvent::Started);
    let _ = ready.send(Ok(thread_id));
    let exit = message_loop(|_| false);

    let failure = match exit {
        MessageLoopExit::Quit => {
            worker.transition(WorkerEvent::MessageLoopQuit);
            None
        }
        MessageLoopExit::Failed(error) => {
            worker.transition(WorkerEvent::MessageLoopFailed);
            Some(error)
        }
    };
    // Clear callback ownership before unhooking so even a stray native call
    // during teardown can only pass through.
    clear_callback();
    // SAFETY: both handles are the live ones returned by SetWindowsHookExW,
    // each unhooked exactly once here as the thread exits.
    unsafe {
        UnhookWindowsHookEx(keyboard_hook);
        UnhookWindowsHookEx(mouse_hook);
    }
    if let Some(error) = failure {
        tracing::error!(error, "Windows hook message loop failed");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MessageLoopExit {
    Quit,
    Failed(u32),
}

fn message_loop(mut handle_thread_message: impl FnMut(&MSG) -> bool) -> MessageLoopExit {
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a live, owned MSG; a null window handle retrieves
        // messages for the calling thread. Returns 0 on WM_QUIT and -1 on error.
        let result = unsafe { GetMessageW(&raw mut msg, std::ptr::null_mut(), 0, 0) };
        if result == 0 {
            return MessageLoopExit::Quit;
        }
        if result < 0 {
            // SAFETY: GetLastError reads the calling thread's last-error code
            // immediately after the failed GetMessageW call.
            return MessageLoopExit::Failed(unsafe { GetLastError() });
        }
        if handle_thread_message(&msg) {
            continue;
        }
        // SAFETY: `msg` was just populated by GetMessageW and outlives the call.
        unsafe { TranslateMessage(&raw const msg) };
        // SAFETY: as above — `msg` is a live, initialized MSG.
        unsafe { DispatchMessageW(&raw const msg) };
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
    if code != HC_ACTION.cast_signed() {
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
        .map_or(EventDisposition::PassThrough, |cb| {
            cb(HookEvent::Mouse(event))
        });
    match disposition {
        EventDisposition::PassThrough => call_next(code, wparam, lparam),
        EventDisposition::Suppress => 1,
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

#[expect(
    clippy::cast_possible_truncation,
    reason = "WPARAM/LPARAM are pointer-sized by ABI but carry 32-bit message payloads"
)]
fn translate_event(wparam: WPARAM, data: MSLLHOOKSTRUCT) -> Option<MouseEvent> {
    // Every mouse message carries the cursor point, so the baseline advances on
    // all of them: injected motion is dropped below but still moves the cursor,
    // and a gesture's button-down seeds the baseline for its own first move.
    let previous = LAST_POINT.replace(Some(data.pt));

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
        // Windows WH_MOUSE_LL does not expose a cheap device identity; leave
        // `device` as None so remapping still works (see hook_runtime: non-macOS
        // keeps remapping when attribution is absent).
        return Some(MouseEvent::Button {
            id,
            pressed,
            device: None,
        });
    }

    match wparam as u32 {
        // A positive high word means the wheel was rotated forward (away from the
        // user). Pass the sign through unchanged so `delta_y > 0` is "scroll up" on
        // every platform — matching macOS (`SCROLL_WHEEL_EVENT_DELTA_AXIS_1`) and
        // Linux (`REL_WHEEL`), whose deltas feed the same direction-sensitive
        // bindings. Negating here flipped scroll-up/-down only on Windows.
        // `from_trackpad` is always false on Windows: the wheel arrives as
        // WM_MOUSEWHEEL and precision-touchpad scrolling as separate input, so
        // a wheel event is unambiguously a mouse wheel (unlike macOS).
        WM_MOUSEWHEEL => Some(MouseEvent::Scroll {
            delta: ScrollDelta::wheel_ticks(
                0.0,
                f64::from(signed_high_word(data.mouseData)) / WHEEL_DELTA,
            ),
            from_trackpad: false,
            device: None,
        }),
        WM_MOUSEHWHEEL => Some(MouseEvent::Scroll {
            delta: ScrollDelta::wheel_ticks(
                f64::from(signed_high_word(data.mouseData)) / WHEEL_DELTA,
                0.0,
            ),
            from_trackpad: false,
            device: None,
        }),
        WM_MOUSEMOVE => {
            let (delta_x, delta_y) = motion_delta(previous?, data.pt)?;
            Some(MouseEvent::Moved { delta_x, delta_y })
        }
        _ => None,
    }
}

/// Relative motion between two cursor points, or `None` for a report that
/// didn't move the cursor — the accumulator downstream sums these deltas
/// against a threshold, so a non-move must not count.
///
/// # Limitation
/// `WH_MOUSE_LL` carries the cursor *position*, which the OS clamps to the
/// desktop bounds, so a swipe that runs into an edge stops producing deltas
/// even though the device keeps reporting counts. A gesture started near an
/// edge can therefore fail to reach the swipe threshold. Reading the device's
/// own counts needs raw input (`WM_INPUT`), a separate message pump.
fn motion_delta(previous: POINT, pt: POINT) -> Option<(i32, i32)> {
    let delta_x = pt.x - previous.x;
    let delta_y = pt.y - previous.y;
    (delta_x != 0 || delta_y != 0).then_some((delta_x, delta_y))
}

/// Low-level keyboard-hook procedure the OS invokes for every key event.
///
/// # Safety
/// Must only be installed as a `WH_KEYBOARD_LL` hook via `SetWindowsHookExW`.
/// When `code == HC_ACTION`, Windows guarantees `lparam` points to a live
/// `KBDLLHOOKSTRUCT`; [`key_hook_data`] relies on that contract to
/// dereference it.
unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code != HC_ACTION.cast_signed() {
        return call_next(code, wparam, lparam);
    }

    // SAFETY: `keyboard_proc` is only installed as a WH_KEYBOARD_LL hook and
    // this is the `code == HC_ACTION` arm, so `lparam` is the live
    // `KBDLLHOOKSTRUCT` pointer `key_hook_data` requires.
    let Some(data) = (unsafe { key_hook_data(lparam) }) else {
        return call_next(code, wparam, lparam);
    };
    let Some(event) = translate_key(wparam, data, current_modifiers()) else {
        return call_next(code, wparam, lparam);
    };

    let callback = CALLBACK.lock().ok().and_then(|slot| slot.clone());
    let disposition = callback
        .as_ref()
        .map_or(EventDisposition::PassThrough, |cb| {
            cb(HookEvent::Key(event))
        });
    match disposition {
        EventDisposition::PassThrough => call_next(code, wparam, lparam),
        EventDisposition::Suppress => 1,
    }
}

/// Copy the `KBDLLHOOKSTRUCT` the OS passed in `lparam`, or `None` if `lparam`
/// is null.
///
/// # Safety
/// `lparam` must be the `lParam` the OS passes to a `WH_KEYBOARD_LL` hook
/// procedure for an `HC_ACTION` event — i.e. it points to a live
/// `KBDLLHOOKSTRUCT` (or is 0). Any other non-zero value is undefined behavior.
unsafe fn key_hook_data(lparam: LPARAM) -> Option<KBDLLHOOKSTRUCT> {
    if lparam == 0 {
        return None;
    }
    // SAFETY: by this function's contract `lparam` is the WH_KEYBOARD_LL
    // HC_ACTION lParam and is non-zero here, so it points to a live
    // `KBDLLHOOKSTRUCT`. We copy it out by value (plain-old-data) and never
    // retain the pointer.
    Some(unsafe { *(lparam as *const KBDLLHOOKSTRUCT) })
}

/// Translate a `WH_KEYBOARD_LL` message into a [`KeyEvent`]. Returns `None`
/// for injected input (our own `SendInput` synthesis must not re-enter the
/// remapper) and for keys outside the remapper's Esc/F1–F19 vocabulary, which
/// pass through without ever reaching the callback.
#[expect(
    clippy::cast_possible_truncation,
    reason = "WPARAM is pointer-sized by ABI but carries a 32-bit message id"
)]
fn translate_key(
    wparam: WPARAM,
    data: KBDLLHOOKSTRUCT,
    modifiers: KeyModifiers,
) -> Option<KeyEvent> {
    if data.flags & LLKHF_INJECTED != 0 {
        return None;
    }
    // Alt-held keys arrive as WM_SYSKEYDOWN/-UP, so `alt+fN` triggers still
    // reach the remapper.
    let pressed = match wparam as u32 {
        WM_KEYDOWN | WM_SYSKEYDOWN => true,
        WM_KEYUP | WM_SYSKEYUP => false,
        _ => return None,
    };
    Some(KeyEvent {
        keycode: mac_keycode(data.vkCode)?,
        pressed,
        modifiers,
    })
}

/// macOS `kVK_*` keycodes for F1–F19 in order — [`KeyEvent`] carries macOS
/// virtual keycodes on every platform, matching the `KeyTrigger` config
/// vocabulary.
const FKEY_MAC_KEYCODES: [u16; 19] = [
    0x7A, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6D, 0x67, 0x6F, 0x69, 0x6B, 0x71, 0x6A,
    0x40, 0x4F, 0x50,
];

/// Map a Windows virtual-key code to the macOS keycode [`KeyEvent`] carries,
/// or `None` for keys outside the Esc/F1–F19 set.
fn mac_keycode(vk: u32) -> Option<u16> {
    let vk = u16::try_from(vk).ok()?;
    if vk == VK_ESCAPE {
        return Some(0x35);
    }
    FKEY_MAC_KEYCODES
        .get(usize::from(vk.checked_sub(VK_F1)?))
        .copied()
}

/// Snapshot the modifier state via `GetAsyncKeyState` — `WH_KEYBOARD_LL`
/// events carry no modifier flags of their own.
fn current_modifiers() -> KeyModifiers {
    KeyModifiers {
        shift: key_held(VK_SHIFT),
        control: key_held(VK_CONTROL),
        option: key_held(VK_MENU),
        command: key_held(VK_LWIN) || key_held(VK_RWIN),
    }
}

fn key_held(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: GetAsyncKeyState takes the key code by value; no preconditions.
    (unsafe { GetAsyncKeyState(i32::from(vk)) }) < 0
}

fn high_word(value: u32) -> u16 {
    (value >> 16) as u16
}

fn signed_high_word(value: u32) -> i16 {
    high_word(value).cast_signed()
}

fn last_error(context: &str) -> HookError {
    // SAFETY: GetLastError reads the calling thread's last-error code; no preconditions.
    let code = unsafe { GetLastError() };
    HookError::WindowsHook(format!("{context} failed with GetLastError={code}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gesture button pressed before the hook has seen any motion used to
    /// leave the baseline empty, so the swipe's first move was swallowed as the
    /// initial sample and a short swipe fell back to a click.
    #[test]
    fn button_down_seeds_the_baseline_for_the_first_move() {
        let at = |x, y| MSLLHOOKSTRUCT {
            pt: POINT { x, y },
            ..MSLLHOOKSTRUCT::default()
        };
        // Explicit, so the test doesn't depend on running on a fresh thread
        // (`--test-threads=1` shares one with every other test).
        LAST_POINT.set(None);
        translate_event(WM_LBUTTONDOWN as WPARAM, at(500, 400));

        assert!(matches!(
            translate_event(WM_MOUSEMOVE as WPARAM, at(560, 395)),
            Some(MouseEvent::Moved {
                delta_x: 60,
                delta_y: -5
            })
        ));
        assert!(
            translate_event(WM_MOUSEMOVE as WPARAM, at(560, 395)).is_none(),
            "a repeated point is not motion"
        );
    }

    #[test]
    fn translate_event_ignores_injected_mouse_input() {
        let data = MSLLHOOKSTRUCT {
            flags: LLMHF_INJECTED,
            ..MSLLHOOKSTRUCT::default()
        };

        assert!(translate_event(WM_LBUTTONDOWN as WPARAM, data).is_none());
        assert!(translate_event(WM_MOUSEMOVE as WPARAM, data).is_none());
    }

    fn key(vk: u16) -> KBDLLHOOKSTRUCT {
        KBDLLHOOKSTRUCT {
            vkCode: u32::from(vk),
            ..KBDLLHOOKSTRUCT::default()
        }
    }

    /// The hook emits macOS `kVK_*` keycodes; a drift from the `KeyTrigger`
    /// parse table in openlogi-core would make every saved binding miss.
    #[test]
    fn emitted_keycodes_match_the_key_trigger_vocabulary() {
        use openlogi_core::config::KeyTrigger;

        let esc: KeyTrigger = "esc".parse().expect("parse key trigger");
        assert_eq!(mac_keycode(u32::from(VK_ESCAPE)), Some(esc.keycode));
        for n in 1..=19u16 {
            let trigger: KeyTrigger = format!("f{n}").parse().expect("parse key trigger");
            assert_eq!(
                mac_keycode(u32::from(VK_F1 + n - 1)),
                Some(trigger.keycode),
                "f{n}"
            );
        }
    }

    #[test]
    fn translate_key_maps_press_and_release() {
        let mods = KeyModifiers::default();
        // VK_F18 = VK_F1 + 17.
        let down = translate_key(WM_KEYDOWN as WPARAM, key(VK_F1 + 17), mods);
        assert!(matches!(
            down,
            Some(KeyEvent {
                keycode: 0x4F,
                pressed: true,
                ..
            })
        ));
        let up = translate_key(WM_KEYUP as WPARAM, key(VK_F1 + 17), mods);
        assert!(matches!(up, Some(KeyEvent { pressed: false, .. })));
    }

    /// Alt-held F-keys arrive as WM_SYSKEYDOWN/-UP; an `alt+fN` trigger must
    /// still see them.
    #[test]
    fn translate_key_handles_syskey_messages() {
        let mods = KeyModifiers {
            option: true,
            ..KeyModifiers::default()
        };
        let down = translate_key(WM_SYSKEYDOWN as WPARAM, key(VK_F1), mods);
        assert!(matches!(
            down,
            Some(KeyEvent {
                keycode: 0x7A,
                pressed: true,
                ..
            })
        ));
        let up = translate_key(WM_SYSKEYUP as WPARAM, key(VK_F1), mods);
        assert!(matches!(up, Some(KeyEvent { pressed: false, .. })));
    }

    #[test]
    fn translate_key_ignores_injected_keyboard_input() {
        let data = KBDLLHOOKSTRUCT {
            vkCode: u32::from(VK_F1),
            flags: LLKHF_INJECTED,
            ..KBDLLHOOKSTRUCT::default()
        };

        assert!(translate_key(WM_KEYDOWN as WPARAM, data, KeyModifiers::default()).is_none());
    }

    /// Ordinary typing keys are outside the Esc/F1–F19 vocabulary and must
    /// never reach the callback — their raw VK codes would otherwise collide
    /// with the macOS keycode space (e.g. VK 0x41 'A' vs kVK 0x41 keypad-.).
    #[test]
    fn translate_key_passes_unmapped_keys_through() {
        let mods = KeyModifiers::default();
        // 'A'
        assert!(translate_key(WM_KEYDOWN as WPARAM, key(0x41), mods).is_none());
        // VK_F20, one past the modeled range.
        assert!(translate_key(WM_KEYDOWN as WPARAM, key(VK_F1 + 19), mods).is_none());
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
        let Some(MouseEvent::Scroll { delta, .. }) =
            translate_event(WM_MOUSEWHEEL as WPARAM, forward)
        else {
            panic!("expected a scroll event");
        };
        assert!(delta.x().abs() < f64::EPSILON);
        assert!(
            delta.y() > 0.0,
            "wheel-forward should scroll up, got {}",
            delta.y()
        );
    }
}
