//! The agent's Windows notification-area (tray) icon.
//!
//! Mirrors the macOS menu-bar item ([`crate::tray`]): the always-on agent
//! hosts the tray, the GUI is on-demand. Without it the app has no visible
//! presence at all once the GUI window is closed — the agent keeps working
//! but the user has no way to tell, or to get the window back (#347).
//!
//! The menu is smaller than macOS's: Settings / About / Check-for-Updates go
//! through `openlogi://` deeplinks there, and Windows has no scheme
//! registration yet — so read-only battery rows, "Show Main Window" (also the
//! left-click action) and "Quit OpenLogi". Show focuses the running GUI if there is one (a
//! second launch would exit on the `openlogi.lock` singleton) or spawns the
//! sibling `OpenLogi.exe` / `openlogi-desktop.exe`. Quit terminates the GUI
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

use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use tracing::{debug, info, warn};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject, GetDC, HBRUSH, ReleaseDC,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CW_USEDEFAULT, CreateIconFromResourceEx, CreateIconIndirect, CreatePopupMenu,
    CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW, EnumWindows, GetCursorPos,
    GetMessageW, GetWindowThreadProcessId, HICON, ICONINFO, IDI_APPLICATION, IsIconic,
    IsWindowVisible, LR_DEFAULTCOLOR, LoadIconW, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG,
    RegisterClassW, RegisterWindowMessageW, SW_RESTORE, SetForegroundWindow, ShowWindow,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP,
    WM_CONTEXTMENU, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
};

/// Tray callback message the icon posts to the hidden window.
const WM_TRAY: u32 = WM_APP + 1;
/// Menu command ids returned by `TrackPopupMenu`.
const ID_SHOW: usize = 1;
const ID_QUIT: usize = 2;
/// Command id shared by every battery row. The rows are `MF_GRAYED`, so
/// `TrackPopupMenu` can never return it; it exists only because `AppendMenuW`
/// requires one.
const ID_BATTERY_ROW: usize = 3;

/// The `TaskbarCreated` broadcast id, resolved once the window exists. Zero
/// until then; real ids are never zero (`RegisterWindowMessageW` starts at
/// 0xC000).
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// Posted by the agent core thread when it wants a balloon shown. `WPARAM` is
/// an owned `*mut Balloon` the receiver takes responsibility for.
const WM_TRAY_BALLOON: u32 = WM_APP + 2;

/// Posted by the agent core thread when the battery glyph should change.
/// `WPARAM` carries the glyph index (see `BatteryGlyph::index`) — a plain
/// scalar, so unlike [`WM_TRAY_BALLOON`] nothing is owned across the boundary.
const WM_TRAY_GLYPH: u32 = WM_APP + 3;

/// `WPARAM` sentinel on [`WM_TRAY_GLYPH`] meaning "restore the brand mark".
/// Glyph indices are small, so this can never collide with one.
const GLYPH_BRAND: WPARAM = WPARAM::MAX;

/// Posted by the agent core thread when the hover text should change.
/// `WPARAM` is an owned `*mut String` the receiver takes responsibility for.
const WM_TRAY_TOOLTIP: u32 = WM_APP + 4;

/// The tray window, published once it exists so the agent core thread can post
/// to it. Zero until then; a post to a null handle is a no-op we tolerate
/// rather than a race we synchronise.
static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);

/// Last glyph index the core thread asked for, so an Explorer restart can
/// restore it instead of silently reverting to the brand mark. `-1` means the
/// brand mark is what belongs on screen.
static LAST_GLYPH: AtomicIsize = AtomicIsize::new(-1);

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
        TRAY_HWND.store(hwnd as isize, Ordering::Relaxed);
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
        WM_TRAY_BALLOON => {
            // SAFETY: `notify` boxed this payload and posted it exactly once;
            // this arm is its only receiver, so reconstituting the box here
            // takes sole ownership and drops it at the end of the arm.
            let balloon = unsafe { Box::from_raw(wparam as *mut Balloon) };
            // SAFETY: `hwnd` is our own window, live for the duration of the
            // callback, and this is the thread that added the icon.
            unsafe { show_balloon(hwnd, &balloon.title, &balloon.body) };
            0
        }
        WM_TRAY_TOOLTIP => {
            // SAFETY: `request_tooltip` boxed this payload and posted it
            // exactly once; this arm is its only receiver, so reconstituting
            // the box takes sole ownership and drops it at the end of the arm.
            let text = unsafe { Box::from_raw(wparam as *mut String) };
            // SAFETY: our own live window, on the thread that owns the icon.
            unsafe { set_tooltip(hwnd, &text) };
            0
        }
        WM_TRAY_GLYPH => {
            // SAFETY: our own live window, on the thread that owns the icon.
            // `tray_icon()` rebuilds the brand mark for the current taskbar
            // theme, which is exactly what `add_tray_icon` installs.
            unsafe {
                let icon = match crate::tray_glyph::BatteryGlyph::from_index(wparam) {
                    Some(glyph) if wparam != GLYPH_BRAND => battery_icon(glyph),
                    _ => tray_icon(),
                };
                set_tray_icon(hwnd, icon);
            }
            0
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            // Explorer restarted; every tray icon was dropped. Re-add ours.
            // SAFETY: `hwnd` is our own window, still live while its window
            // procedure runs, and this is the thread that created it — the
            // same conditions under which `run_tray_loop` first added the icon.
            unsafe { add_tray_icon(hwnd) };
            // `add_tray_icon` always installs the brand mark, so a battery
            // glyph would silently revert on an Explorer restart without this.
            let last = LAST_GLYPH.load(Ordering::Relaxed);
            if let Ok(index) = usize::try_from(last)
                && let Some(glyph) = crate::tray_glyph::BatteryGlyph::from_index(index)
            {
                // SAFETY: same live window and owning thread as above.
                unsafe {
                    set_tray_icon(hwnd, battery_icon(glyph));
                }
            }
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
        // Seeded from the live snapshot, not a constant: an Explorer restart
        // re-adds the icon and would otherwise drop the battery lines.
        copy_truncated(
            &mut nid.szTip,
            &crate::tray_battery::tooltip(&crate::tray_battery::snapshot()),
        );
        if Shell_NotifyIconW(NIM_ADD, &raw const nid) == 0 {
            warn!("Shell_NotifyIconW(NIM_ADD) failed — no tray icon");
        }
    }
}

/// Attach an informational balloon to the existing icon.
///
/// Title and body are truncated to the fixed `NOTIFYICONDATAW` buffers (64 and
/// 256 UTF-16 units including the terminator). Truncating a long device name
/// is better than dropping the alert.
///
/// Focus Assist / Do Not Disturb can swallow a `NIF_INFO` balloon with no
/// error to report, so a successful call here is not proof anything appeared
/// on screen. There is nothing to do about it from this side — worth knowing
/// before someone reports that alerts never fire.
unsafe fn show_balloon(hwnd: HWND, title: &str, body: &str) {
    // SAFETY: `nid` is fully initialized below and both buffers are bounded by
    // construction.
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_INFO;
        copy_truncated(&mut nid.szInfoTitle, title);
        copy_truncated(&mut nid.szInfo, body);
        if Shell_NotifyIconW(NIM_MODIFY, &raw const nid) == 0 {
            warn!("Shell_NotifyIconW(NIM_MODIFY) failed — low-battery alert not shown");
        } else {
            info!("balloon handed to the shell");
        }
    }
}

/// Copy `text` into a fixed win32 UTF-16 buffer, truncating to leave room for
/// the NUL terminator.
fn copy_truncated(buffer: &mut [u16], text: &str) {
    let limit = buffer.len().saturating_sub(1);
    let encoded: Vec<u16> = text.encode_utf16().take(limit).collect();
    buffer[..encoded.len()].copy_from_slice(&encoded);
    buffer[encoded.len()] = 0;
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

/// Edge length to render the battery icon at: whatever the shell says a small
/// icon is right now (`SM_CXSMICON`, 16px at 100% DPI, more when scaled).
///
/// Rendering at the display size rather than a fixed 32px matters for these
/// glyphs specifically: they are 2px strokes, and handing the shell an
/// oversized bitmap makes it resample a second time, which smears the stroke
/// into a soft grey smudge next to the crisp system icons.
fn icon_size() -> usize {
    // SAFETY: a pure metric query with no arguments to get wrong.
    let px = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
            windows_sys::Win32::UI::WindowsAndMessaging::SM_CXSMICON,
        )
    };
    usize::try_from(px).unwrap_or(16).clamp(16, 64)
}

/// Render a Lucide battery glyph into a 32x32 premultiplied BGRA buffer.
///
/// The vendored SVGs are `stroke="currentColor"` outlines, so recolouring for
/// the taskbar theme is a string substitution before parsing — black on a
/// light taskbar, white on a dark one, exactly as the brand mark behaves.
///
/// `resvg` produces premultiplied RGBA, which is already what an alpha icon's
/// colour bitmap wants; only the channel order needs swapping.
///
/// Returns `None` when the SVG cannot be parsed or the pixmap cannot be
/// allocated; the caller falls back to the brand mark rather than show nothing.
fn battery_pixels(
    glyph: crate::tray_glyph::BatteryGlyph,
    light: bool,
    edge: usize,
) -> Option<Vec<u32>> {
    let colour = if light { "#000000" } else { "#ffffff" };
    let mut svg = glyph.asset().replace("currentColor", colour);

    // Optical correction for the notification area. Lucide draws a 2px stroke
    // on a 24px viewBox; at the 16px the shell actually shows, that lands at
    // 1.33px and antialiases into a soft grey smudge beside the crisp system
    // glyphs. Thickening the stroke so it covers whole pixels is Lucide's own
    // advice for small sizes, and it is what makes the icon read as solid
    // white rather than grey.
    if edge <= 20 {
        svg = svg.replace(r#"stroke-width="2""#, r#"stroke-width="3""#);
    }

    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).ok()?;
    let size = u32::try_from(edge).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "edge is a small icon metric, well inside f32's exact integer range"
    )]
    let scale = edge as f32 / tree.size().width();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    Some(
        pixmap
            .pixels()
            .iter()
            .map(|px| {
                u32::from(px.alpha()) << 24
                    | u32::from(px.red()) << 16
                    | u32::from(px.green()) << 8
                    | u32::from(px.blue())
            })
            .collect(),
    )
}

/// Turn a BGRA pixel buffer into an `HICON`.
///
/// Uses `CreateDIBSection` rather than `CreateCompatibleBitmap` + `FillRect`:
/// GDI's drawing calls do not write the alpha channel, so a compatible bitmap
/// filled that way yields an icon that is transparent or garbage. A DIB
/// section hands back the raw pixels, which the caller has already computed.
unsafe fn icon_from_pixels(pixels: &[u32], edge: usize) -> HICON {
    debug_assert_eq!(pixels.len(), edge * edge);
    // SAFETY: every handle created here is released before returning, and the
    // DIB's pixel buffer is written only while the section is alive.
    unsafe {
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = edge as i32;
        // Negative height: top-down rows, matching the buffer's row order.
        info.bmiHeader.biHeight = -(edge as i32);
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;

        let screen = GetDC(std::ptr::null_mut());
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let colour = CreateDIBSection(
            screen,
            &raw const info,
            DIB_RGB_COLORS,
            &raw mut bits,
            std::ptr::null_mut(),
            0,
        );
        ReleaseDC(std::ptr::null_mut(), screen);
        if colour.is_null() || bits.is_null() {
            warn!("battery glyph DIB allocation failed — falling back to the brand mark");
            return tray_icon();
        }
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u32>(), pixels.len());

        // A 32bpp icon takes its transparency from the alpha channel, so the
        // AND mask is all zeros ("show the colour pixel"). 1bpp rows are
        // WORD-aligned; 32 px is 4 bytes, already aligned.
        // 1bpp rows are WORD-aligned; a row of `edge` bits rounds up to
        // whole bytes, then to an even count.
        let stride = edge.div_ceil(8).next_multiple_of(2);
        let mask_bits = vec![0u8; stride * edge];
        let mask = CreateBitmap(edge as i32, edge as i32, 1, 1, mask_bits.as_ptr().cast());

        let mut icon_info: ICONINFO = std::mem::zeroed();
        icon_info.fIcon = 1;
        icon_info.hbmMask = mask;
        icon_info.hbmColor = colour;
        let icon = CreateIconIndirect(&raw const icon_info);

        DeleteObject(colour.cast());
        DeleteObject(mask.cast());

        if icon.is_null() {
            warn!("battery glyph rasterisation failed — falling back to the brand mark");
            return tray_icon();
        }
        icon
    }
}

/// A cached battery `HICON` for one (glyph, theme) pair.
///
/// Handles are built once and kept for the process lifetime — at most
/// 6 glyphs x 2 themes = 12 — rather than rebuilt and destroyed on every
/// change. See [`set_tray_icon`] for why nothing is freed.
fn battery_icon(glyph: crate::tray_glyph::BatteryGlyph) -> HICON {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    /// Cache key: glyph index, light-taskbar flag, and render size. The value
    /// is the `HICON` as an `isize` so the map stays `Send`. Size is part of
    /// the key because a DPI change moves `SM_CXSMICON` under a live process.
    type IconCache = Mutex<HashMap<(usize, bool, usize), isize>>;

    static CACHE: OnceLock<IconCache> = OnceLock::new();
    let light = taskbar_is_light();
    let edge = icon_size();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = match cache.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let handle = *guard
        .entry((glyph.index(), light, edge))
        .or_insert_with(|| {
            let Some(pixels) = battery_pixels(glyph, light, edge) else {
                warn!("battery glyph render failed — falling back to the brand mark");
                // SAFETY: builds the brand icon exactly as `add_tray_icon` does.
                return (unsafe { tray_icon() }) as isize;
            };
            // SAFETY: the buffer is exactly `edge * edge` entries, and every
            // GDI handle the call allocates is released inside it.
            (unsafe { icon_from_pixels(&pixels, edge) }) as isize
        });
    handle as HICON
}

/// Point the existing icon at a different `HICON`.
///
/// The handles come from a process-lifetime cache and are deliberately never
/// destroyed: a `DestroyIcon` racing a paint or a balloon that still
/// references the handle is a use-after-free, and the cache is bounded at a
/// couple of dozen handles.
unsafe fn set_tray_icon(hwnd: HWND, icon: HICON) {
    // SAFETY: `nid` is fully initialized below; `icon` outlives the call.
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_ICON;
        nid.hIcon = icon;
        if Shell_NotifyIconW(NIM_MODIFY, &raw const nid) == 0 {
            warn!("Shell_NotifyIconW(NIM_MODIFY) failed — tray glyph not updated");
        }
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
        // Battery rows first: they are what the user opened the menu to read.
        // Disabled, because there is nothing to open — the GUI is one item
        // below. An empty snapshot appends nothing at all, leaving the menu
        // identical to the one shipped before this feature.
        let batteries = crate::tray_battery::snapshot();
        for device in &batteries {
            AppendMenuW(
                menu,
                MF_STRING | MF_GRAYED,
                ID_BATTERY_ROW,
                wide(&crate::tray_battery::label(device)).as_ptr(),
            );
        }
        if !batteries.is_empty() {
            AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
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
    info!("tray Quit — exiting agent");
    #[expect(
        clippy::exit,
        reason = "reached from the window procedure on the tray thread: the status cannot travel back through an `extern \"system\"` callback, and ending the message pump would only end this thread while `main` keeps running the agent core"
    )]
    std::process::exit(0);
}

/// Show a low-battery notification as a balloon on the tray icon.
///
/// Windows renders a `NIF_INFO` balloon as an ordinary toast, and because it
/// rides an icon the agent already owns there is no `AppUserModelID` or
/// Start-Menu shortcut to register — which a toast raised any other way would
/// require. The trade is that the notification cannot outlive the icon: with
/// `show_in_menu_bar = false` there is no icon and no alert.
///
/// Called from the agent core thread, so the payload is boxed and handed to
/// the tray thread, which owns every `Shell_NotifyIconW` call.
pub fn notify(title: &str, body: &str) {
    let hwnd = TRAY_HWND.load(Ordering::Relaxed);
    if hwnd == 0 {
        // No tray icon (disabled by preference, or not installed yet) — there
        // is nothing to attach a balloon to.
        info!(title, body, "low-battery alert suppressed: no tray icon");
        return;
    }
    let payload = Box::into_raw(Box::new(Balloon {
        title: title.to_string(),
        body: body.to_string(),
    }));
    // SAFETY: `hwnd` was published by the tray thread after a successful
    // CreateWindowExW. The boxed payload is claimed by exactly one receiver —
    // the WM_TRAY_BALLOON arm in `wnd_proc` — which reconstitutes and drops
    // it. If the post fails the box is reclaimed here instead, so the pointer
    // is never leaked and never freed twice.
    let posted = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
            hwnd as HWND,
            WM_TRAY_BALLOON,
            payload as WPARAM,
            0,
        )
    };
    if posted != 0 {
        info!(title, body, "tray notification posted");
    } else {
        warn!("could not post the low-battery alert to the tray thread");
        // SAFETY: the post failed, so no receiver will ever claim this
        // pointer; reclaiming it here is the only way it is freed.
        drop(unsafe { Box::from_raw(payload) });
    }
}

/// Notification payload handed across the thread boundary.
struct Balloon {
    title: String,
    body: String,
}

/// Ask the tray thread to replace the icon's hover text.
///
/// Called from the agent core thread, and only when the text actually changed
/// (`tray_battery::publish` filters repeats).
pub fn request_tooltip(text: String) {
    let hwnd = TRAY_HWND.load(Ordering::Relaxed);
    if hwnd == 0 {
        // No icon to label yet. `add_tray_icon` seeds its own tip from the
        // live snapshot, but the cache must not claim this text reached the
        // shell, or the next identical tick is filtered as a repeat.
        crate::tray_battery::invalidate();
        return;
    }
    let payload = Box::into_raw(Box::new(text));
    // SAFETY: `hwnd` was published by the tray thread after a successful
    // CreateWindowExW. The boxed payload is claimed by exactly one receiver —
    // the WM_TRAY_TOOLTIP arm in `wnd_proc` — which reconstitutes and drops
    // it. A failed post reclaims it here, so it is never leaked or double-freed.
    let posted = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
            hwnd as HWND,
            WM_TRAY_TOOLTIP,
            payload as WPARAM,
            0,
        )
    };
    if posted == 0 {
        warn!("could not post the tray tooltip to the tray thread");
        // SAFETY: the post failed, so no receiver will claim this pointer.
        drop(unsafe { Box::from_raw(payload) });
        crate::tray_battery::invalidate();
    }
}

/// Replace the icon's hover text in place.
unsafe fn set_tooltip(hwnd: HWND, text: &str) {
    // SAFETY: `nid` is fully initialized below and the tip buffer is bounded
    // by `copy_truncated`.
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_TIP;
        copy_truncated(&mut nid.szTip, text);
        if Shell_NotifyIconW(NIM_MODIFY, &raw const nid) == 0 {
            warn!("Shell_NotifyIconW(NIM_MODIFY) failed — tray tooltip not updated");
        } else {
            // The Windows 11 taskbar renders tooltips in its own XAML surface,
            // which no screen capture or window enumeration can inspect. This
            // is the only place the exact accepted string is observable.
            debug!(text, "tray tooltip accepted by the shell");
        }
    }
}

/// Ask the tray thread to swap its icon.
///
/// Called from the agent core thread, and only when the icon actually changed
/// (`tray_glyph::publish` filters repeats), so this is a rare event rather
/// than a per-tick one.
pub fn request_icon(icon: crate::tray_glyph::TrayIcon) {
    use crate::tray_glyph::TrayIcon;

    // One match decides everything the swap needs: the message payload, the
    // value the Explorer-restart path replays, and what to log.
    let (wparam, lparam, replay) = match icon {
        TrayIcon::Brand => {
            info!("tray icon changed back to the brand mark");
            (GLYPH_BRAND, 0, -1)
        }
        TrayIcon::Battery(glyph) => {
            info!(?glyph, "tray icon changed to the battery glyph");
            let index = glyph.index();
            (index, 0, index as isize)
        }
    };
    // Recorded even when there is no window yet, so an icon installed later —
    // or re-added after an Explorer restart — picks up the right glyph.
    LAST_GLYPH.store(replay, Ordering::Relaxed);

    let hwnd = TRAY_HWND.load(Ordering::Relaxed);
    if hwnd == 0 {
        // `LAST_GLYPH` above still records the intent, but nothing reached the
        // shell, so the dedup cache must not claim otherwise.
        crate::tray_glyph::invalidate();
        return;
    }
    // SAFETY: `hwnd` was published by the tray thread after a successful
    // CreateWindowExW, and the payload is two scalars — nothing is owned
    // across the boundary, so a dropped message leaks nothing.
    let posted = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
            hwnd as HWND,
            WM_TRAY_GLYPH,
            wparam,
            lparam,
        )
    };
    if posted == 0 {
        warn!("could not post the tray glyph to the tray thread");
        crate::tray_glyph::invalidate();
    }
}

/// NUL-terminated UTF-16 for win32 W-APIs.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        reason = "a vendored SVG that fails to render is a broken build, and the panic names which glyph"
    )]

    use super::{battery_pixels, copy_truncated, is_gui_process_name};

    /// Fixed render size for the pixel tests, so they do not depend on the
    /// machine's DPI the way `icon_size()` deliberately does.
    const TEST_EDGE: usize = 32;
    use crate::tray_glyph::BatteryGlyph;

    /// Count of pixels that are not fully transparent.
    fn painted(pixels: &[u32]) -> usize {
        pixels.iter().filter(|px| *px >> 24 != 0).count()
    }

    #[test]
    fn every_glyph_renders_a_full_icon_sized_buffer() {
        for glyph in BatteryGlyph::ALL {
            let pixels = battery_pixels(glyph, true, TEST_EDGE).expect("vendored SVG must render");
            assert_eq!(
                pixels.len(),
                TEST_EDGE * TEST_EDGE,
                "{glyph:?} rendered the wrong buffer size"
            );
            assert!(
                painted(&pixels) > 0,
                "{glyph:?} rendered nothing — the recolour probably broke the SVG"
            );
        }
    }

    #[test]
    fn the_glyph_follows_the_taskbar_theme() {
        // The icons are monochrome strokes, so "which theme" is readable from
        // any painted pixel: dark taskbar paints white, light paints black.
        let on_light = battery_pixels(BatteryGlyph::Full, true, TEST_EDGE).expect("renders");
        let on_dark = battery_pixels(BatteryGlyph::Full, false, TEST_EDGE).expect("renders");

        let brightest = |px: &[u32]| {
            px.iter()
                .filter(|p| *p >> 24 != 0)
                .map(|p| p & 0x00ff_ffff)
                .max()
                .expect("something was painted")
        };
        assert_eq!(
            brightest(&on_light),
            0x0000_0000,
            "light taskbar draws black"
        );
        assert_ne!(brightest(&on_dark), 0x0000_0000, "dark taskbar draws white");
    }

    #[test]
    fn the_background_is_fully_transparent() {
        let pixels = battery_pixels(BatteryGlyph::Full, true, TEST_EDGE).expect("renders");
        assert_eq!(pixels[0], 0, "the corner must not paint a box");
        assert!(
            painted(&pixels) < pixels.len(),
            "an outline icon must leave most of the canvas empty"
        );
    }

    #[test]
    fn the_glyphs_are_visually_distinct() {
        // A rendering bug that fell back to one asset for everything would
        // still pass every other test here.
        let rendered: Vec<Vec<u32>> = BatteryGlyph::ALL
            .into_iter()
            .map(|glyph| battery_pixels(glyph, true, TEST_EDGE).expect("renders"))
            .collect();
        for (i, a) in rendered.iter().enumerate() {
            for (j, b) in rendered.iter().enumerate().skip(i + 1) {
                assert_ne!(
                    a,
                    b,
                    "{:?} and {:?} rendered identically",
                    BatteryGlyph::ALL[i],
                    BatteryGlyph::ALL[j]
                );
            }
        }
    }

    #[test]
    fn the_glyph_index_round_trips() {
        for glyph in BatteryGlyph::ALL {
            assert_eq!(
                BatteryGlyph::from_index(glyph.index()),
                Some(glyph),
                "{glyph:?} must survive the WPARAM round trip"
            );
        }
        assert_eq!(
            BatteryGlyph::from_index(BatteryGlyph::ALL.len()),
            None,
            "an out-of-range index must not decode to a glyph"
        );
    }

    #[test]
    fn a_short_title_is_terminated_right_after_its_text() {
        let mut buffer = [0xffffu16; 8];
        copy_truncated(&mut buffer, "ok");
        assert_eq!(buffer[0], u16::from(b'o'));
        assert_eq!(buffer[1], u16::from(b'k'));
        assert_eq!(buffer[2], 0);
    }

    #[test]
    fn a_long_title_is_truncated_and_still_nul_terminated() {
        let mut buffer = [0xffffu16; 8];
        copy_truncated(&mut buffer, "abcdefghijkl");
        assert_eq!(
            &buffer[..7],
            &"abcdefg".encode_utf16().collect::<Vec<_>>()[..]
        );
        assert_eq!(buffer[7], 0, "the buffer must stay NUL-terminated");
    }

    #[test]
    fn the_cli_binary_is_not_the_gui() {
        assert!(is_gui_process_name("OpenLogi.exe"));
        assert!(is_gui_process_name("openlogi-desktop.exe"));
        assert!(!is_gui_process_name("openlogi.exe")); // the CLI
    }
}
