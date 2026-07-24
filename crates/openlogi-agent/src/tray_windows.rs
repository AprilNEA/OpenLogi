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
//! sibling `OpenLogi.exe` / `openlogi-gui.exe`. Quit terminates the GUI
//! first — a surviving GUI's IPC retry loop would immediately respawn the
//! agent we are quitting — then exits.
//!
//! Everything runs on one dedicated thread: the hidden window, its message
//! pump, and the menu. The icon is re-added when Explorer restarts (the
//! `TaskbarCreated` broadcast), and the glyph tracks the taskbar theme
//! (black on a light taskbar, white on a dark one) at install time.

#![expect(
    unsafe_code,
    reason = "raw win32: Shell_NotifyIconW + a hidden window's message pump — localized here"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "win32 message plumbing round-trips ids through WPARAM/LPARAM by design"
)]

use std::sync::atomic::{AtomicU32, Ordering};

use tracing::{info, warn};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::HBRUSH;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CW_USEDEFAULT, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyMenu, DispatchMessageW, EnumWindows, GetCursorPos, GetMessageW,
    GetWindowThreadProcessId, HICON, IDI_APPLICATION, IsIconic, IsWindowVisible, LR_DEFAULTCOLOR,
    LoadIconW, MF_SEPARATOR, MF_STRING, MSG, RegisterClassW, RegisterWindowMessageW, SW_RESTORE,
    SetForegroundWindow, ShowWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
    TranslateMessage, WM_APP, WM_CONTEXTMENU, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
    WS_OVERLAPPED,
};

/// Tray callback message the icon posts to the hidden window.
const WM_TRAY: u32 = WM_APP + 1;
/// Menu command ids returned by `TrackPopupMenu`.
const ID_SHOW: usize = 1;
const ID_QUIT: usize = 2;

/// The `TaskbarCreated` broadcast id, resolved once the window exists. Zero
/// until then; real ids are never zero (`RegisterWindowMessageW` starts at
/// 0xC000).
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// Host the tray icon on its own thread. No-op when the user disabled the
/// menu-bar/tray preference (same `show_in_menu_bar` setting macOS honors;
/// takes effect on the agent's next launch, as there).
///
/// Failures are logged, never fatal — the agent's real work (hook, HID++,
/// IPC) must not die because a shell icon couldn't be installed.
pub fn spawn(show_in_tray: bool) {
    if !show_in_tray {
        info!("tray icon disabled by preference — agent stays invisible");
        return;
    }
    if let Err(e) = std::thread::Builder::new()
        .name("openlogi-tray".into())
        .spawn(run_tray_loop)
    {
        warn!(error = %e, "could not spawn the tray thread");
    }
}

/// Create the hidden window, install the icon, and pump messages for the
/// agent's lifetime.
fn run_tray_loop() {
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
            wide("OpenLogi Agent").as_ptr(),
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
                WM_RBUTTONUP | WM_CONTEXTMENU => unsafe { show_menu(hwnd) },
                _ => {}
            }
            0
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            // Explorer restarted; every tray icon was dropped. Re-add ours.
            unsafe { add_tray_icon(hwnd) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Install the icon (idempotent enough for the re-add path: a duplicate
/// `NIM_ADD` fails silently and the existing icon stays).
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
        AppendMenuW(menu, MF_STRING, ID_SHOW, wide("Show Main Window").as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, ID_QUIT, wide("Quit OpenLogi").as_ptr());

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
/// portable layout) or `openlogi-gui.exe` (cargo target dir).
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
///
/// The `OpenLogi.exe` match is case-*sensitive*: the CLI binary is
/// `openlogi.exe`, which case-insensitively equals the GUI's `OpenLogi.exe`.
/// The two share a name on the case-insensitive filesystem (only the dev
/// target dir ever holds both), so an `eq_ignore_ascii_case` match would take
/// a transient CLI invocation for the GUI and then fail to find its window.
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
            let name = p.name().to_string_lossy();
            (name == "OpenLogi.exe" || name.eq_ignore_ascii_case("openlogi-gui.exe"))
                && (own_user.is_none() || p.user_id() == own_user)
        })
        .map(|p| p.pid().as_u32())
        .collect()
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
    // `openlogi-gui.exe`, and the CLI shares `OpenLogi.exe`'s name on the
    // case-insensitive filesystem — so `dir.join("OpenLogi.exe").exists()`
    // there resolves to the CLI and would launch it. Probing the unambiguous
    // `openlogi-gui.exe` first avoids that; the installed layout has only
    // `OpenLogi.exe` and falls through to it.
    let gui = ["openlogi-gui.exe", "OpenLogi.exe"]
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
    info!("tray Quit — exiting agent");
    std::process::exit(0);
}

/// NUL-terminated UTF-16 for win32 W-APIs.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
