//! The agent's Windows notification-area (tray) icon.
//!
//! Mirrors the macOS menu-bar item ([`crate::tray`]): the always-on agent
//! hosts the tray, the GUI is on-demand. Without it the app has no visible
//! presence at all once the GUI window is closed — the agent keeps working
//! but the user has no way to tell, or to get the window back (#347).
//!
//! The menu is smaller than macOS's: Settings / About / Check-for-Updates go
//! through `openlogi://` deeplinks there, and Windows has no scheme
//! registration yet — so just "Show Main Window" (also the left-click action)
//! and "Quit OpenLogi". Show focuses the running GUI if there is one (a
//! second launch would exit on the `openlogi.lock` singleton) or spawns the
//! sibling `OpenLogi.exe` / `openlogi-desktop.exe`. Quit asks the GUI to
//! close cleanly first — a surviving GUI's IPC retry loop would immediately
//! respawn the agent we are quitting — and retains a bounded hard-stop
//! fallback before the agent exits.
//!
//! Everything runs on one dedicated thread: the hidden window, its message
//! pump, and the menu. The icon is re-added when Explorer restarts (the
//! `TaskbarCreated` broadcast), and the glyph tracks the taskbar theme
//! (black on a light taskbar, white on a dark one) at install time.

#![expect(
    unsafe_code,
    reason = "raw win32: Shell_NotifyIconW + a hidden window's message pump — localized here"
)]
use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use tracing::{info, warn};
use windows_sys::Win32::Foundation::{FILETIME, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::HBRUSH;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    QueryFullProcessImageNameW, TerminateProcess,
};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CW_USEDEFAULT, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyMenu, DispatchMessageW, EnumWindows, GetCursorPos, GetMessageW,
    GetWindowThreadProcessId, HICON, IDI_APPLICATION, IsIconic, IsWindowVisible, LR_DEFAULTCOLOR,
    LoadIconW, MF_SEPARATOR, MF_STRING, MSG, RegisterClassW, RegisterWindowMessageW, SW_RESTORE,
    SetForegroundWindow, ShowWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
    TranslateMessage, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP,
    WNDCLASSW, WS_OVERLAPPED,
};

use crate::shutdown::{self, ShutdownRequestSender};

/// Tray callback message the icon posts to the hidden window.
const WM_TRAY: u32 = WM_APP + 1;
/// Menu command ids returned by `TrackPopupMenu`.
const ID_SHOW: usize = 1;
const ID_QUIT: usize = 2;
/// How long tray Quit gives the GUI to run its normal teardown before using
/// the existing hard-stop fallback.
const GRACEFUL_QUIT_TIMEOUT: Duration = Duration::from_secs(2);
const GRACEFUL_QUIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The `TaskbarCreated` broadcast id, resolved once the window exists. Zero
/// until then; real ids are never zero (`RegisterWindowMessageW` starts at
/// 0xC000).
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

thread_local! {
    /// Where the win32 tray callback hands process termination to the async
    /// lifecycle. The callback and message pump share this one tray thread.
    static SHUTDOWN_TX: RefCell<Option<ShutdownRequestSender>> = const { RefCell::new(None) };
}

/// Host the tray icon on its own thread. No-op when the user disabled the
/// menu-bar/tray preference (same `show_in_menu_bar` setting macOS honors;
/// takes effect on the agent's next launch, as there).
///
/// Failures are logged, never fatal — the agent's real work (hook, HID++,
/// IPC) must not die because a shell icon couldn't be installed.
pub fn spawn(show_in_tray: bool, shutdown_tx: ShutdownRequestSender) {
    if !show_in_tray {
        info!("tray icon disabled by preference — agent stays invisible");
        return;
    }
    if let Err(e) = std::thread::Builder::new()
        .name("openlogi-tray".into())
        .spawn(move || run_tray_loop(shutdown_tx))
    {
        warn!(error = %e, "could not spawn the tray thread");
    }
}

/// Create the hidden window, install the icon, and pump messages for the
/// agent's lifetime.
fn run_tray_loop(shutdown_tx: ShutdownRequestSender) {
    SHUTDOWN_TX.with_borrow_mut(|slot| *slot = Some(shutdown_tx));
    let class_name = wide("OpenLogiAgentTray");
    // SAFETY: plain win32 registration/creation calls with pointers that
    // outlive the calls; the class name buffer lives until thread exit.
    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut::<core::ffi::c_void>() as HBRUSH,
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        if RegisterClassW(&raw const wc) == 0 {
            warn!("tray window class registration failed — no tray icon");
            return;
        }
        // A normal (never-shown) top-level window, not message-only: only
        // top-level windows receive the TaskbarCreated broadcast that tells
        // us to re-add the icon after an Explorer restart.
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            wide(openlogi_core::brand::Helper::Agent.display_name()).as_ptr(),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            warn!("tray window creation failed — no tray icon");
            return;
        }
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            Ordering::Relaxed,
        );
        add_tray_icon(hwnd);
        info!("tray icon installed");

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&raw mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }
}

#[expect(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "WM_TRAY packs the mouse message id into the low bits of LPARAM"
)]
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAY => {
            match lparam as u32 {
                WM_LBUTTONUP => open_or_focus_gui(),
                // SAFETY: win32 dispatches this callback on the tray thread —
                // the one that created `hwnd` and pumps its messages — so the
                // handle is live for the whole call, and `TrackPopupMenu` gets
                // the owning thread it requires.
                WM_RBUTTONUP | WM_CONTEXTMENU => unsafe { show_menu(hwnd) },
                _ => {}
            }
            0
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            // Explorer restarted; every tray icon was dropped. Re-add ours.
            // SAFETY: `hwnd` is our own window, still live while its window
            // procedure runs, and this is the thread that created it — the
            // same conditions under which `run_tray_loop` first added the icon.
            unsafe { add_tray_icon(hwnd) };
            0
        }
        // SAFETY: handing the system back the message it just delivered,
        // unchanged: `hwnd` is live for the duration of the callback and
        // `wparam`/`lparam` are the payload win32 paired with `msg`, which is
        // exactly what the default handler expects.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Install the icon (idempotent enough for the re-add path: a duplicate
/// `NIM_ADD` fails silently and the existing icon stays).
#[expect(
    clippy::cast_possible_truncation,
    reason = "NOTIFYICONDATAW is a few hundred bytes"
)]
unsafe fn add_tray_icon(hwnd: HWND) {
    // SAFETY: `nid` is fully initialized below; the tip buffer is bounded.
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = tray_icon();
        let tip = wide("OpenLogi");
        nid.szTip[..tip.len()].copy_from_slice(&tip);
        if Shell_NotifyIconW(NIM_ADD, &raw const nid) == 0 {
            warn!("Shell_NotifyIconW(NIM_ADD) failed — no tray icon");
        }
    }
}

/// The tray glyph: the brand mark in black on a light taskbar, white on a
/// dark one (`SystemUsesLightTheme`, default dark). Both variants are the
/// macOS status-item asset; `CreateIconFromResourceEx` accepts raw PNG
/// buffers (the same PNG-compressed form .ico files carry since Vista).
/// Falls back to the stock application icon rather than showing nothing.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the PNG is embedded at build time and is a few kilobytes"
)]
unsafe fn tray_icon() -> HICON {
    const BLACK: &[u8] = include_bytes!("../assets/tray-icon@2x.png");
    const WHITE: &[u8] = include_bytes!("../assets/tray-icon-white@2x.png");
    let png: &[u8] = if taskbar_is_light() { BLACK } else { WHITE };
    // SAFETY: the buffer is a valid embedded PNG; the call copies it.
    let icon = unsafe {
        CreateIconFromResourceEx(
            png.as_ptr(),
            png.len() as u32,
            1, // fIcon (not a cursor)
            0x0003_0000,
            0, // cx/cy 0: use the resource's own size
            0,
            LR_DEFAULTCOLOR,
        )
    };
    if icon.is_null() {
        warn!("tray icon PNG rejected — falling back to the stock icon");
        // SAFETY: loading a stock system icon.
        unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) }
    } else {
        icon
    }
}

/// Whether the taskbar renders light (needs the black glyph). Missing value
/// means the Windows default: dark.
fn taskbar_is_light() -> bool {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_value::<u32, _>("SystemUsesLightTheme"))
        .is_ok_and(|v| v == 1)
}

/// Show the context menu at the cursor and run the chosen command.
#[expect(
    clippy::cast_sign_loss,
    reason = "TrackPopupMenu returns the command id it was given, never negative"
)]
unsafe fn show_menu(hwnd: HWND) {
    // SAFETY: menu handles are created and destroyed here; the
    // SetForegroundWindow/WM_NULL bracket is the documented TrackPopupMenu
    // dance for tray menus (without it the menu won't dismiss on outside
    // clicks).
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        // Built fresh on every right-click, so `t!` follows a live language
        // switch with no rebuild plumbing.
        AppendMenuW(
            menu,
            MF_STRING,
            ID_SHOW,
            wide(&rust_i18n::t!("app.show_main_window")).as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(
            menu,
            MF_STRING,
            ID_QUIT,
            wide(&rust_i18n::t!("app.quit_openlogi")).as_ptr(),
        );

        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&raw mut pt);
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);

        match cmd as usize {
            ID_SHOW => open_or_focus_gui(),
            ID_QUIT => quit(hwnd),
            _ => {}
        }
    }
}

/// Focus the running GUI's window, or launch the sibling GUI binary when no
/// GUI is running (a second launch would just exit on the `openlogi.lock`
/// singleton, so spawning blindly does nothing visible).
fn open_or_focus_gui() {
    let processes = gui_processes();
    if processes.is_empty() {
        spawn_gui();
        return;
    }
    let pids: Vec<_> = processes.iter().map(|process| process.pid).collect();
    if !focus_window_of(&pids) {
        // Running but windowless should not happen (the GUI always has its
        // main window); log rather than spawn a doomed duplicate.
        warn!("GUI process is running but no window was found to focus");
    }
}

/// PIDs of this user's running GUI processes: `OpenLogi.exe` (installed /
/// portable layout) or `openlogi-desktop.exe` (cargo target dir).
///
/// Matching by *name* rather than by install directory is deliberate: the
/// GUI is a per-user singleton (`openlogi.lock` lives under the profile), so
/// whichever copy is running — MSI, portable, dev — it is the only one that
/// *can* run, it is the one talking to this agent (the IPC pipe name is
/// machine-global), and a directory-scoped Show would spawn a sibling that
/// immediately loses the singleton and exits, doing nothing visible. The
/// same-user filter keeps other sessions (fast user switching) out of
/// Show/Quit — their windows are invisible to `EnumWindows` and their
/// processes unkillable anyway, but don't even consider them.
struct GuiProcess {
    pid: u32,
    started_at: u64,
    handle: Option<OwnedHandle>,
}

fn gui_processes() -> Vec<GuiProcess> {
    use sysinfo::{Pid, Process, ProcessesToUpdate, System};
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    let own_user = system
        .process(Pid::from_u32(std::process::id()))
        .and_then(Process::user_id);
    system
        .processes()
        .values()
        .filter(|p| {
            is_gui_process_name(&p.name().to_string_lossy())
                && (own_user.is_none() || p.user_id() == own_user)
        })
        .map(|process| {
            let mut target = GuiProcess {
                pid: process.pid().as_u32(),
                started_at: process.start_time(),
                handle: None,
            };
            // Keep sysinfo's snapshot (and its process handles) alive while
            // acquiring our handle, so the identified PID cannot be recycled.
            target.handle = open_gui_process(&target, process.name());
            target
        })
        .collect()
}

/// Whether a process image name is one of the GUI binaries.
///
/// `OpenLogi.exe` is matched case-*sensitively*: the CLI is `openlogi.exe`,
/// which `eq_ignore_ascii_case` would accept, and the dev target dir holds
/// both. Windows reports image names with their on-disk case, so this holds.
fn is_gui_process_name(name: &str) -> bool {
    name == "OpenLogi.exe" || name.eq_ignore_ascii_case("openlogi-desktop.exe")
}

/// Bring the first visible top-level window owned by one of `pids` to the
/// foreground, restoring it if minimized. Returns whether one was found.
fn focus_window_of(pids: &[u32]) -> bool {
    struct Search<'a> {
        pids: &'a [u32],
        focused: bool,
    }
    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: lparam is the &mut Search passed to EnumWindows below and
        // outlives the enumeration; the win32 queries take a valid hwnd.
        unsafe {
            let search = &mut *(lparam as *mut Search<'_>);
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &raw mut pid);
            if search.pids.contains(&pid) && IsWindowVisible(hwnd) != 0 {
                if IsIconic(hwnd) != 0 {
                    ShowWindow(hwnd, SW_RESTORE);
                }
                SetForegroundWindow(hwnd);
                search.focused = true;
                return 0; // stop enumerating
            }
            1
        }
    }
    let mut search = Search {
        pids,
        focused: false,
    };
    // SAFETY: the callback only dereferences the &mut Search for the duration
    // of this call.
    unsafe {
        EnumWindows(Some(enum_proc), std::ptr::addr_of_mut!(search) as LPARAM);
    }
    search.focused
}

/// Ask visible top-level windows to close and return the PIDs whose close
/// request was queued, so fallback diagnostics describe each process accurately.
fn request_gui_close(pids: &[u32]) -> Vec<u32> {
    struct Search<'a> {
        pids: &'a [u32],
        requested: Vec<u32>,
    }
    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: lparam is the &mut Search passed to EnumWindows below and
        // outlives the enumeration; the win32 queries take a valid hwnd.
        unsafe {
            let search = &mut *(lparam as *mut Search<'_>);
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &raw mut pid);
            if search.pids.contains(&pid)
                && IsWindowVisible(hwnd) != 0
                && windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(hwnd, WM_CLOSE, 0, 0)
                    != 0
                && !search.requested.contains(&pid)
            {
                search.requested.push(pid);
            }
            1
        }
    }
    let mut search = Search {
        pids,
        requested: Vec::new(),
    };
    // SAFETY: the callback only dereferences the &mut Search for the duration
    // of this call.
    unsafe {
        EnumWindows(Some(enum_proc), std::ptr::addr_of_mut!(search) as LPARAM);
    }
    search.requested
}

/// Open and validate the process once, then retain its kernel identity through
/// termination. A PID lookup after validation could target a replacement.
fn open_gui_process(target: &GuiProcess, expected_name: &OsStr) -> Option<OwnedHandle> {
    // sysinfo reports zero when it could not open a process handle. Without
    // that pinned identity, do not act on a potentially recycled PID.
    if target.started_at == 0 {
        return None;
    }
    // SAFETY: OpenProcess accepts a numeric PID and returns an owned handle.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION
                | PROCESS_TERMINATE
                | windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE,
            0,
            target.pid,
        )
    };
    if raw.is_null() {
        return None;
    }
    // SAFETY: this non-null OpenProcess result is owned exactly once here.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: the owned process handle is live; all outputs are writable.
    if unsafe {
        GetProcessTimes(
            raw,
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    } == 0
    {
        return None;
    }
    // Process enumeration and handle acquisition are separate operations.
    // Validate the image on the retained handle, not only the snapshot's name.
    let mut image = vec![0u16; 32_768];
    let mut length = 32_768;
    // SAFETY: the owned handle is valid and length describes the writable buffer.
    if unsafe { QueryFullProcessImageNameW(raw, 0, image.as_mut_ptr(), &raw mut length) } == 0 {
        return None;
    }
    let image = std::path::PathBuf::from(OsString::from_wide(&image[..length as usize]));
    if image.file_name() != Some(expected_name) {
        return None;
    }
    // sysinfo exposes creation time as whole Unix seconds, unlike FILETIME.
    let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    let started_at = (ticks / 10_000_000).checked_sub(11_644_473_600)?;
    (started_at == target.started_at).then_some(handle)
}

fn any_process_running(targets: &[(u32, OwnedHandle)]) -> bool {
    targets.iter().any(|(_, handle)| process_running(handle))
}

fn process_running(handle: &OwnedHandle) -> bool {
    use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    // SAFETY: the process handle remains owned throughout this probe.
    unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) == WAIT_TIMEOUT }
}

/// Wait for all target processes to exit. The injected probes keep the
/// bounded wait and fallback decision independently testable.
fn wait_for_exit_with(
    timeout: Duration,
    mut any_running: impl FnMut() -> bool,
    mut pause: impl FnMut(Duration),
) -> bool {
    let deadline = Instant::now() + timeout;
    while any_running() {
        if Instant::now() >= deadline {
            return false;
        }
        pause(GRACEFUL_QUIT_POLL_INTERVAL);
    }
    true
}

/// Launch the GUI binary sitting next to the agent.
fn spawn_gui() {
    let Ok(exe) = std::env::current_exe() else {
        warn!("could not resolve the agent's own path — cannot launch the GUI");
        return;
    };
    let Some(dir) = exe.parent() else { return };
    // Dev target dir first: it holds both `openlogi.exe` (CLI) and
    // `openlogi-desktop.exe`, and the CLI shares `OpenLogi.exe`'s name on the
    // case-insensitive filesystem — so `dir.join("OpenLogi.exe").exists()`
    // there resolves to the CLI and would launch it. Probing the unambiguous
    // `openlogi-desktop.exe` first avoids that; the installed layout has only
    // `OpenLogi.exe` and falls through to it.
    let gui = ["openlogi-desktop.exe", "OpenLogi.exe"]
        .iter()
        .map(|name| dir.join(name))
        .find(|p| p.exists());
    let Some(gui) = gui else {
        warn!(dir = %dir.display(), "no GUI binary found next to the agent");
        return;
    };
    match std::process::Command::new(&gui).spawn() {
        Ok(_) => info!(path = %gui.display(), "tray — launched the GUI"),
        Err(e) => warn!(error = %e, path = %gui.display(), "tray — could not launch the GUI"),
    }
}

/// Quit the whole app: ask the GUI to exit cleanly first (its IPC retry loop
/// would otherwise respawn the agent we are about to exit), force-stop it only
/// after a bounded wait, then remove the icon and exit the agent.
#[expect(
    clippy::cast_possible_truncation,
    reason = "NOTIFYICONDATAW is a few hundred bytes"
)]
fn quit(hwnd: HWND) {
    let processes: Vec<_> = gui_processes()
        .into_iter()
        .filter_map(|target| {
            if target.handle.is_none() {
                warn!(
                    pid = target.pid,
                    "tray Quit — could not retain GUI process; skipping it"
                );
            }
            target.handle.map(|handle| (target.pid, handle))
        })
        .collect();
    let pids: Vec<_> = processes.iter().map(|(pid, _)| *pid).collect();
    let close_requested = request_gui_close(&pids);
    if !close_requested.is_empty() {
        info!(
            count = close_requested.len(),
            "tray Quit — requested graceful GUI shutdown"
        );
        if wait_for_exit_with(
            GRACEFUL_QUIT_TIMEOUT,
            || any_process_running(&processes),
            std::thread::sleep,
        ) {
            info!("tray Quit — GUI exited gracefully");
        }
    }

    for (pid, handle) in processes {
        if !process_running(&handle) {
            continue;
        }
        // SAFETY: terminate the same kernel process retained before WM_CLOSE,
        // never a fresh process found by looking its PID up again.
        if unsafe { TerminateProcess(handle.as_raw_handle(), 1) } != 0 {
            // TerminateProcess is asynchronous; wait before the agent exits so
            // GUI teardown and singleton release have a chance to finish.
            if !wait_for_exit_with(
                GRACEFUL_QUIT_TIMEOUT,
                || process_running(&handle),
                std::thread::sleep,
            ) {
                warn!(
                    pid,
                    "tray Quit — GUI termination did not finish within the timeout"
                );
            } else if close_requested.contains(&pid) {
                warn!(
                    pid,
                    "tray Quit — graceful shutdown timed out; terminated the GUI"
                );
            } else {
                warn!(
                    pid,
                    "tray Quit — no close request was sent; terminated the GUI"
                );
            }
        } else {
            warn!(pid, "tray Quit — could not terminate the GUI");
        }
    }
    // SAFETY: removing the icon this thread added.
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        Shell_NotifyIconW(NIM_DELETE, &raw const nid);
    }
    crate::overlay::evict_on_quit();
    info!("tray Quit — requesting graceful agent shutdown");
    let requests = SHUTDOWN_TX.with_borrow(Clone::clone);
    shutdown::request_tray_quit(requests.as_ref(), 0);
}

/// NUL-terminated UTF-16 for win32 W-APIs.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::os::windows::io::AsRawHandle;
    use std::time::Duration;

    use super::{
        GuiProcess, is_gui_process_name, open_gui_process, process_running, wait_for_exit_with,
    };

    #[test]
    fn the_cli_binary_is_not_the_gui() {
        assert!(is_gui_process_name("OpenLogi.exe"));
        assert!(is_gui_process_name("openlogi-desktop.exe"));
        assert!(!is_gui_process_name("openlogi.exe")); // the CLI
    }

    #[test]
    fn process_handle_rejects_a_mismatched_creation_time() {
        let target = GuiProcess {
            pid: std::process::id(),
            started_at: 1,
            handle: None,
        };
        let executable = std::env::current_exe().unwrap();
        assert!(open_gui_process(&target, executable.file_name().unwrap()).is_none());
    }

    #[test]
    fn retained_handle_tracks_exit_without_resolving_the_pid_again() {
        use std::process::Command;
        use sysinfo::{Pid, ProcessesToUpdate, System};

        let mut child = Command::new("cmd.exe")
            .args(["/C", "pause"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::All, true);
        let process = system.process(Pid::from_u32(child.id())).unwrap();
        let target = GuiProcess {
            pid: child.id(),
            started_at: process.start_time(),
            handle: None,
        };
        assert!(open_gui_process(&target, std::ffi::OsStr::new("OpenLogi.exe")).is_none());
        let handle = open_gui_process(&target, process.name()).unwrap();
        assert!(process_running(&handle));
        // SAFETY: the retained handle refers only to this test's child.
        let terminated = unsafe { super::TerminateProcess(handle.as_raw_handle(), 1) };
        assert_ne!(terminated, 0);
        child.wait().unwrap();
        assert!(!process_running(&handle));
    }

    #[test]
    fn graceful_quit_stops_waiting_after_the_process_exits() {
        let probes = Cell::new(0);
        let exited = wait_for_exit_with(
            Duration::from_secs(1),
            || {
                let probe = probes.get() + 1;
                probes.set(probe);
                probe < 3
            },
            |_| {},
        );

        assert!(exited);
        assert_eq!(probes.get(), 3);
    }

    #[test]
    fn graceful_quit_reports_timeout_for_the_force_kill_fallback() {
        let exited = wait_for_exit_with(Duration::ZERO, || true, |_| {});

        assert!(!exited);
    }
}
