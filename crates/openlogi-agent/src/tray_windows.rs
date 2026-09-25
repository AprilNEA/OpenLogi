//! The agent's Windows notification-area (tray) icon.
//!
//! Mirrors the macOS menu-bar item ([`crate::tray`]): the always-on agent
//! hosts the tray, the GUI is on-demand. Without it the app has no visible
//! presence at all once the GUI window is closed — the agent keeps working
//! but the user has no way to tell, or to get the window back (#347).
//!
//! The menu is smaller than macOS's: Settings / About / Check-for-Updates go
//! through `openlogi://` deeplinks there, and Windows has no scheme
//! registration yet. Device battery rows sit between "Show Main Window"
//! (also the left-click action) and "Quit OpenLogi". Show focuses the running GUI if there is one (a
//! second launch would exit on the `openlogi.lock` singleton) or spawns the
//! sibling `OpenLogi.exe` / `openlogi-desktop.exe`. Quit terminates the GUI
//! first — a surviving GUI's IPC retry loop would immediately respawn the
//! agent we are quitting — then exits.
//!
//! Everything runs on one dedicated thread: the hidden window, its message
//! pump, and the menu. The icon is re-added when Explorer restarts (the
//! `TaskbarCreated` broadcast), and the glyph tracks the taskbar theme
//! (black on a light taskbar, white on a dark one), with orange/red warnings.
//! Device rows are owner-drawn inside the native menu with MSAA labels.

#![expect(
    unsafe_code,
    reason = "raw win32: Shell_NotifyIconW + a hidden window's message pump — localized here"
)]
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use tracing::{info, warn};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::HBRUSH;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_STATE, NIF_TIP, NIIF_WARNING, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIS_HIDDEN, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CW_USEDEFAULT, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DispatchMessageW, EnumWindows, GetCursorPos, GetMessageW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, RegisterClassW,
    RegisterWindowMessageW, SW_RESTORE, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    SetForegroundWindow, SetWindowPos, ShowWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, TranslateMessage, WM_APP, WM_CONTEXTMENU, WM_DRAWITEM, WM_LBUTTONUP,
    WM_MEASUREITEM, WM_NULL, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_THEMECHANGED, WNDCLASSW,
    WS_OVERLAPPED,
};

use crate::shutdown::{self, ShutdownRequestSender};

mod battery;
mod icon;

const WM_BATTERY: u32 = WM_APP + 2;
const WM_BATTERY_NOTIFY: u32 = WM_APP + 3;
static TRAY_HWND: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

// Adapted from yuzi-co’s PR #964: keep payloads in process-owned memory.
// Window messages are bare wakeups, never pointers supplied by other processes.
struct Balloon {
    title: String,
    body: String,
}
static BALLOONS: Mutex<VecDeque<Balloon>> = Mutex::new(VecDeque::new());

/// Tray callback message the icon posts to the hidden window.
const WM_TRAY: u32 = WM_APP + 1;
/// Menu command ids returned by `TrackPopupMenu`.
const ID_SHOW: usize = 1;
const ID_QUIT: usize = 2;

/// The `TaskbarCreated` broadcast id, resolved once the window exists. Zero
/// until then; real ids are never zero (`RegisterWindowMessageW` starts at
/// 0xC000).
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static VISIBLE: Cell<bool> = const { Cell::new(true) };
    static CURRENT_ICON: RefCell<Option<icon::Icon>> = const { RefCell::new(None) };
    /// The callback hands termination to the async lifecycle on this tray thread.
    static SHUTDOWN_TX: RefCell<Option<ShutdownRequestSender>> = const { RefCell::new(None) };
}

/// Host the tray icon on its own thread. The disabled tray remains hidden so
/// independent battery notifications still have a Windows notification source.
///
/// Failures are logged, never fatal — the agent's real work (hook, HID++,
/// IPC) must not die because a shell icon couldn't be installed.
pub fn spawn(show_in_tray: bool, shutdown_tx: ShutdownRequestSender) {
    if let Err(e) = std::thread::Builder::new()
        .name("openlogi-tray".into())
        .spawn(move || run_tray_loop(show_in_tray, shutdown_tx))
    {
        warn!(error = %e, "could not spawn the tray thread");
    }
}

/// Create the hidden window, install the icon, and pump messages for the
/// agent's lifetime.
fn run_tray_loop(show_in_tray: bool, shutdown_tx: ShutdownRequestSender) {
    VISIBLE.set(show_in_tray);
    SHUTDOWN_TX.with_borrow_mut(|slot| *slot = Some(shutdown_tx));
    let class_name = wide("OpenLogiAgentTray");
    // SAFETY: plain win32 registration/creation calls with pointers that
    // outlive the calls; the class name buffer lives until thread exit.
    unsafe {
        windows_sys::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(
            windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
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
        TRAY_HWND.store(hwnd, Ordering::Release);
        add_tray_icon(hwnd);
        drain_balloons(hwnd);
        info!("tray icon installed");

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&raw mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
        TRAY_HWND.store(std::ptr::null_mut(), Ordering::Release);
        CURRENT_ICON.with_borrow_mut(|icon| *icon = None);
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
        WM_BATTERY | WM_SETTINGCHANGE | WM_THEMECHANGED => {
            // SAFETY: own tray window, on its message-pump thread.
            unsafe { update_tray_icon(hwnd, NIM_MODIFY) };
            0
        }
        WM_BATTERY_NOTIFY => {
            drain_balloons(hwnd);
            0
        }
        WM_MEASUREITEM => {
            // SAFETY: the OS supplies the measurement struct for this message.
            if unsafe { battery::measure(lparam) } {
                1
            } else {
                // SAFETY: unchanged OS message and live window.
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
        WM_DRAWITEM => {
            // SAFETY: the OS supplies the drawing struct and DC for this message.
            if unsafe { battery::draw(lparam) } {
                1
            } else {
                // SAFETY: unchanged OS message and live window.
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
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

/// Install or restore the current brand color after Explorer restarts.
unsafe fn add_tray_icon(hwnd: HWND) {
    // SAFETY: same live tray window and owning thread as the caller.
    unsafe {
        update_tray_icon(hwnd, NIM_ADD);
    }
}

unsafe fn update_tray_icon(hwnd: HWND, operation: u32) {
    let icon = icon::Icon::new(crate::battery::snapshot().severity);
    // SAFETY: fully initialized notification data; icon remains owned until
    // replaced successfully. Windows receives only bounded NUL-terminated text.
    unsafe {
        let mut nid = NOTIFYICONDATAW {
            cbSize: u32::try_from(size_of::<NOTIFYICONDATAW>()).unwrap_or(0),
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON,
            hIcon: icon.handle,
            ..NOTIFYICONDATAW::default()
        };
        if operation == NIM_ADD {
            nid.uFlags |= NIF_MESSAGE | NIF_TIP | NIF_STATE;
            nid.uCallbackMessage = WM_TRAY;
            nid.dwStateMask = NIS_HIDDEN;
            nid.dwState = if VISIBLE.get() { 0 } else { NIS_HIDDEN };
            copy_truncated(&mut nid.szTip, "OpenLogi");
        }
        if Shell_NotifyIconW(operation, &raw const nid) == 0 {
            warn!(operation, "could not update the Windows tray icon");
        } else {
            CURRENT_ICON.with_borrow_mut(|slot| *slot = Some(icon));
        }
    }
}

/// Refresh the tray from the shared snapshot, from any agent thread.
pub fn battery_changed() {
    wake_tray(WM_BATTERY);
}

/// Queue one native battery notification. The tray thread owns shell interaction.
pub fn notify_battery(alert: &crate::battery::Alert) {
    balloons().push_back(Balloon {
        title: alert.title(),
        body: alert.body(),
    });
    wake_tray(WM_BATTERY_NOTIFY);
}

fn wake_tray(message: u32) {
    let hwnd = TRAY_HWND.load(Ordering::Acquire);
    if hwnd.is_null() {
        return;
    } // startup reads the snapshot and drains alerts
    // SAFETY: no message payload pointers; the atomic publishes our live window.
    if unsafe { PostMessageW(hwnd, message, 0, 0) } == 0 {
        warn!(message, "could not wake the Windows tray thread");
    }
}

fn balloons() -> std::sync::MutexGuard<'static, VecDeque<Balloon>> {
    BALLOONS.lock().unwrap_or_else(|error| {
        warn!("recovering the battery notification queue after a thread panic");
        error.into_inner()
    })
}

fn drain_balloons(hwnd: HWND) {
    loop {
        let next = balloons().pop_front();
        let Some(balloon) = next else {
            break;
        };
        // SAFETY: called on the tray thread for its live window. Drop the queue
        // lock before calling into the shell, which may run nested messages.
        unsafe {
            let mut nid = NOTIFYICONDATAW {
                cbSize: u32::try_from(size_of::<NOTIFYICONDATAW>()).unwrap_or(0),
                hWnd: hwnd,
                uID: 1,
                uFlags: NIF_INFO,
                dwInfoFlags: NIIF_WARNING,
                ..NOTIFYICONDATAW::default()
            };
            copy_truncated(&mut nid.szInfoTitle, &balloon.title);
            copy_truncated(&mut nid.szInfo, &balloon.body);
            if Shell_NotifyIconW(NIM_MODIFY, &raw const nid) == 0 {
                warn!("Windows rejected a low-battery notification");
            }
        }
    }
}

/// Preserve complete Unicode scalars and reserve the final UTF-16 NUL.
fn copy_truncated(buffer: &mut [u16], text: &str) {
    buffer.fill(0);
    let mut used = 0;
    for character in text.chars() {
        let mut units = [0; 2];
        let encoded = character.encode_utf16(&mut units);
        if used + encoded.len() >= buffer.len() {
            break;
        }
        buffer[used..used + encoded.len()].copy_from_slice(encoded);
        used += encoded.len();
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
    // TrackPopupMenu runs a nested message loop. Ignore repeated tray clicks
    // until the current menu releases its owner-draw/MSAA row storage.
    if battery::is_open() {
        return;
    }
    // SAFETY: menu handles are created and destroyed here; the
    // SetForegroundWindow/WM_NULL bracket is the documented TrackPopupMenu
    // dance for tray menus (without it the menu won't dismiss on outside
    // clicks).
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&raw mut pt);
        // The never-shown window follows the clicked monitor so its DPI and
        // native menu font match the taskbar which opened this menu.
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            pt.x,
            pt.y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let devices = battery::DeviceMenu::new(hwnd);
        // Built fresh on every right-click, so `t!` follows a live language
        // switch with no rebuild plumbing.
        AppendMenuW(
            menu,
            MF_STRING,
            ID_SHOW,
            wide(&rust_i18n::t!("app.show_main_window")).as_ptr(),
        );
        if let Some(devices) = &devices {
            devices.append(menu);
        }
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(
            menu,
            MF_STRING,
            ID_QUIT,
            wide(&rust_i18n::t!("app.quit_openlogi")).as_ptr(),
        );

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
        battery::clear();

        match cmd as usize {
            ID_SHOW => open_or_focus_gui(),
            ID_QUIT => quit(hwnd),
            id => {
                if let Some(devices) = devices {
                    devices.activate(id);
                }
            }
        }
    }
}

/// Focus the running GUI's window, or launch the sibling GUI binary when no
/// GUI is running (a second launch would just exit on the `openlogi.lock`
/// singleton, so spawning blindly does nothing visible).
fn open_or_focus_gui() {
    let pids = gui_pids();
    if pids.is_empty() {
        spawn_gui();
        return;
    }
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
fn gui_pids() -> Vec<u32> {
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
        .map(|p| p.pid().as_u32())
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

/// Quit the whole app: GUI first (its IPC retry loop would otherwise respawn
/// the agent we are about to exit), then the icon, then the agent. Mirrors
/// the macOS Quit semantics; the GUI holds no unsaved state (config writes
/// are immediate).
#[expect(
    clippy::cast_possible_truncation,
    reason = "NOTIFYICONDATAW is a few hundred bytes"
)]
fn quit(hwnd: HWND) {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    for pid in gui_pids() {
        if let Some(process) = system.process(Pid::from_u32(pid)) {
            if process.kill() {
                info!(pid, "tray Quit — terminated the GUI");
            } else {
                warn!(pid, "tray Quit — could not terminate the GUI");
            }
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
    use super::{copy_truncated, is_gui_process_name};

    #[test]
    fn the_cli_binary_is_not_the_gui() {
        assert!(is_gui_process_name("OpenLogi.exe"));
        assert!(is_gui_process_name("openlogi-desktop.exe"));
        assert!(!is_gui_process_name("openlogi.exe")); // the CLI
    }

    #[test]
    fn notification_truncation_preserves_surrogate_pairs_and_nul() {
        let mut short = [99; 4];
        copy_truncated(&mut short, "AB😀C");
        assert_eq!(short, [65, 66, 0, 0]);
        let mut exact = [99; 5];
        copy_truncated(&mut exact, "AB😀C");
        assert_eq!(String::from_utf16(&exact[..4]).unwrap(), "AB😀");
        assert_eq!(exact[4], 0);
        copy_truncated(&mut [], "ignored");
    }
}
