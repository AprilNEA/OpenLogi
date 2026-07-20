//! The on-screen Action Ring overlay.
//!
//! Interaction model (specified, not guessed): a pad tap opens the ring
//! centred at the cursor; the pointer then moves normally (never captured);
//! a second tap fires the sector the cursor points toward and closes the
//! ring. Cancel: the centre ✕ (click or second tap in the dead zone), `Esc`,
//! or a click outside the ring.
//!
//! The window is a borderless, transparent, always-on-top popup that never
//! takes focus — the action must land in the app the user was using, so
//! focus must not move. GPUI's `WindowKind::PopUp` provides the borderless
//! toolwindow; the no-activate + topmost bits and all show/hide/positioning
//! go through the raw HWND (see [`platform_win`]), because the pinned gpui
//! Windows backend exposes neither. The window is created once on first use
//! and then re-positioned and re-shown per open — no per-tap surface setup.
//!
//! Sector *selection* is plain cursor geometry: the angle from the ring's
//! centre to the global cursor picks the sector, the centre dead zone
//! cancels. The cursor may leave the little overlay window entirely ("move
//! toward" can overshoot) — that changes nothing, since the watcher reads the
//! global cursor, not window-local hover.

use std::time::Duration;

use gpui::{
    App, AppContext as _, Bounds, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Point, Render, StatefulInteractiveElement as _, Styled as _, Window,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, div, point, prelude::FluentBuilder as _,
    px, size,
};
use gpui_component::{ActiveTheme as _, Icon};
use openlogi_core::binding::{Action, RingSlot};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::ipc_client::Command;
use crate::mouse_model::picker::action_icon_path;
use crate::state::AppState;

/// Logical size of the overlay window: wider than tall because side-sector
/// labels sit *beside* the ring (the Options+ layout). Compact enough to fit
/// near screen edges in most positions; when it can't,
/// [`platform_win::show_at`] clamps it into the work area.
const RING_W: f32 = 480.;
/// See [`RING_W`].
const RING_H: f32 = 312.;
/// Radius of the circle the eight sector buttons sit on, from the centre.
const SECTOR_RADIUS: f32 = 96.;
/// Diameter of one sector's circular icon button.
const SECTOR_BUTTON: f32 = 44.;
/// Diameter of the centre ✕ cancel button.
const CENTER_BUTTON: f32 = 30.;
/// Gap between a sector button's outer edge and its label pill.
const LABEL_GAP: f32 = 8.;
/// Cursor distance (logical px) below which a confirm means "cancel" — the
/// dead zone around the centre ✕.
const DEADZONE: f32 = 38.;
/// How often the open-ring watcher samples the global cursor (highlight) and
/// the Esc / outside-click cancel signals.
const WATCH_TICK: Duration = Duration::from_millis(33);

// The ring's fixed palette, deliberately theme-independent and light-on-dark
// — the inverse of the Options+ light-mode reference, because the ring floats
// over a desktop that lives in dark mode: bright circles, dark label pills.
/// Sector button fill.
const PLATE: u32 = 0x00f7_f7f9;
/// Icon strokes on an unaimed button; label pill fill.
const INK: u32 = 0x001c_1c1e;
/// Aimed icon strokes and aimed-pill text.
const WHITE: u32 = 0x00ff_ffff;
/// Label pill text.
const PILL_TEXT: u32 = 0x00f2_f2f4;
/// Centre ✕ fill.
const CROSS_BG: u32 = 0x002c_2c2e;
/// Centre ✕ fill while the cursor aims at the dead zone.
const CROSS_BG_AIMED: u32 = 0x0045_4549;
/// Centre ✕ glyph.
const CROSS_TEXT: u32 = 0x00c9_c9ce;

/// What the cursor currently points at, driven from the global cursor by the
/// watcher so it works even when the pointer is outside the overlay window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Aim {
    /// Inside the dead zone (or no cursor data): a confirm cancels.
    #[default]
    Center,
    /// Toward a sector: a confirm fires its action.
    Sector(RingSlot),
}

/// Pick the aim for a cursor at `cursor` when the ring is centred at
/// `center`, both in the same (physical) coordinate space, with the dead
/// zone scaled by `scale`. Pure, so the sector math is unit-testable.
fn aim_for(center: Point<f32>, cursor: Point<f32>, scale: f32) -> Aim {
    let dx = cursor.x - center.x;
    let dy = cursor.y - center.y;
    if (dx * dx + dy * dy).sqrt() < DEADZONE * scale {
        return Aim::Center;
    }
    // Angle clockwise from straight up, matching `RingSlot::angle_degrees`.
    let degrees = dx.atan2(-dy).to_degrees().rem_euclid(360.);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "degrees is in [0, 360), so the sector index is in 0..=8"
    )]
    let index = (((degrees + 22.5) / 45.).floor() as usize) % 8;
    Aim::Sector(RingSlot::ALL[index])
}

/// The overlay's app-global controller: the singleton window, whether it is
/// currently shown, and the plumbing to fire the selected action.
pub struct RingOverlay {
    commands: mpsc::UnboundedSender<Command>,
    window: Option<WindowHandle<RingView>>,
    open: bool,
    /// Physical-pixel centre of the open ring, for global-cursor hit tests.
    center: Point<f32>,
    /// Physical-per-logical scale of the display the ring opened on.
    scale: f32,
    /// Generation counter; bumping it retires the previous open's watcher.
    epoch: u64,
}

impl gpui::Global for RingOverlay {}

/// Install the controller. Call once at startup, before the first press can
/// arrive.
pub fn init(commands: mpsc::UnboundedSender<Command>, cx: &mut App) {
    cx.set_global(RingOverlay {
        commands,
        window: None,
        open: false,
        center: point(0., 0.),
        scale: 1.,
        epoch: 0,
    });
}

/// Handle one Action Ring pad press from the agent: open the ring when it is
/// closed, confirm the aimed selection when it is open.
pub fn on_pad_press(cx: &mut App) {
    if cx.global::<RingOverlay>().open {
        confirm(cx);
    } else {
        open(cx);
    }
}

/// Open the ring centred at the cursor.
fn open(cx: &mut App) {
    let Some(cursor) = platform_win::cursor_pos() else {
        // Non-Windows builds have no overlay plumbing yet; the press is
        // captured and bindable, the on-screen ring is Windows-first.
        info!("action ring press ignored — no overlay support on this platform yet");
        return;
    };

    let slots = cx.global::<AppState>().ring_slots_for_current();
    let window = match cx.global::<RingOverlay>().window {
        Some(window) => {
            let refreshed = window.update(cx, |view, _, cx| {
                view.slots.clone_from(&slots);
                view.aim = Aim::Center;
                cx.notify();
            });
            if refreshed.is_err() {
                // The window was closed out from under us (display change,
                // gpui teardown) — recreate below.
                cx.global_mut::<RingOverlay>().window = None;
            }
            match cx.global::<RingOverlay>().window {
                Some(window) => window,
                None => match create_window(slots, cx) {
                    Some(window) => window,
                    None => return,
                },
            }
        }
        None => match create_window(slots, cx) {
            Some(window) => window,
            None => return,
        },
    };

    let Some(hwnd) = window_hwnd(&window, cx) else {
        warn!("action ring window has no HWND — cannot show the overlay");
        return;
    };
    let scale = platform_win::window_scale(hwnd);
    // The centre can differ from the cursor near a screen edge (work-area
    // clamp) — the hit-test must aim from the ring's real centre.
    let center = platform_win::show_at(hwnd, cursor, RING_W * scale, RING_H * scale);

    let overlay = cx.global_mut::<RingOverlay>();
    overlay.window = Some(window);
    overlay.open = true;
    overlay.center = center;
    overlay.scale = scale;
    overlay.epoch += 1;
    let epoch = overlay.epoch;
    debug!(x = center.x, y = center.y, "action ring opened");

    // While open: track the global cursor for the sector highlight, and close
    // on Esc or a click outside the window — inputs a no-activate window
    // never receives as messages.
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(WATCH_TICK).await;
            if cx.update(|cx| watch_tick(epoch, hwnd, cx)) {
                break;
            }
        }
    })
    .detach();
}

/// One watcher pass. Returns `true` when this watcher is done (ring closed or
/// superseded by a newer open).
fn watch_tick(epoch: u64, hwnd: isize, cx: &mut App) -> bool {
    {
        let overlay = cx.global::<RingOverlay>();
        if !overlay.open || overlay.epoch != epoch {
            return true;
        }
    }
    if platform_win::escape_pressed() || platform_win::clicked_outside(hwnd) {
        debug!("action ring cancelled (esc / outside click)");
        close(cx);
        return true;
    }
    let (center, scale) = {
        let overlay = cx.global::<RingOverlay>();
        (overlay.center, overlay.scale)
    };
    let aim =
        platform_win::cursor_pos().map_or(Aim::Center, |cursor| aim_for(center, cursor, scale));
    let window = cx.global::<RingOverlay>().window;
    if let Some(window) = window {
        let _ = window.update(cx, |view, _, cx| {
            if view.aim != aim {
                view.aim = aim;
                cx.notify();
            }
        });
    }
    false
}

/// Confirm the current aim: fire the aimed sector's action, or cancel from
/// the dead zone.
fn confirm(cx: &mut App) {
    let (center, scale) = {
        let overlay = cx.global::<RingOverlay>();
        (overlay.center, overlay.scale)
    };
    let aim =
        platform_win::cursor_pos().map_or(Aim::Center, |cursor| aim_for(center, cursor, scale));
    match aim {
        Aim::Center => debug!("action ring cancelled (centre tap)"),
        Aim::Sector(slot) => fire(slot, cx),
    }
    close(cx);
}

/// Fire `slot`'s bound action through the agent.
fn fire(slot: RingSlot, cx: &mut App) {
    let window = cx.global::<RingOverlay>().window;
    let action = window.and_then(|window| {
        window
            .update(cx, |view, _, _| view.action_for(slot))
            .ok()
            .flatten()
    });
    let Some(action) = action else {
        return;
    };
    info!(slot = %slot, action = %action.label(), "action ring → action");
    if cx
        .global::<RingOverlay>()
        .commands
        .send(Command::ExecuteAction(action))
        .is_err()
    {
        warn!("IPC client is gone — ring action dropped");
    }
}

/// Hide the ring.
fn close(cx: &mut App) {
    let overlay = cx.global_mut::<RingOverlay>();
    overlay.open = false;
    overlay.epoch += 1;
    let window = overlay.window;
    if let Some(window) = window
        && let Some(hwnd) = window_hwnd(&window, cx)
    {
        platform_win::hide(hwnd);
    }
}

/// Create the overlay window (hidden — [`platform_win::show_at`] reveals it).
fn create_window(slots: Vec<(RingSlot, Action)>, cx: &mut App) -> Option<WindowHandle<RingView>> {
    let bounds = Bounds {
        origin: point(px(0.), px(0.)),
        size: size(px(RING_W), px(RING_H)),
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: None,
        app_id: Some("openlogi".to_string()),
        kind: WindowKind::PopUp,
        is_movable: false,
        focus: false,
        show: false,
        window_background: gpui::WindowBackgroundAppearance::Transparent,
        ..WindowOptions::default()
    };
    let opened = cx.open_window(options, |_, cx| {
        cx.new(|_| RingView {
            slots,
            aim: Aim::Center,
        })
    });
    match opened {
        Ok(window) => {
            if let Some(hwnd) = window_hwnd(&window, cx) {
                platform_win::apply_overlay_style(hwnd);
                platform_win::clear_accent(hwnd);
            }
            Some(window)
        }
        Err(e) => {
            warn!(error = %e, "could not open the action ring window");
            None
        }
    }
}

/// The raw Win32 handle of a gpui window (`None` off Windows).
fn window_hwnd(window: &WindowHandle<RingView>, cx: &mut App) -> Option<isize> {
    window
        .update(cx, |_, window, _| {
            use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
            match window.window_handle().ok()?.as_raw() {
                RawWindowHandle::Win32(handle) => Some(isize::from(handle.hwnd)),
                _ => None,
            }
        })
        .ok()
        .flatten()
}

/// The ring's root view: eight sector buttons on a circle and the centre ✕.
pub struct RingView {
    slots: Vec<(RingSlot, Action)>,
    aim: Aim,
}

impl RingView {
    fn action_for(&self, slot: RingSlot) -> Option<Action> {
        self.slots
            .iter()
            .find(|(s, _)| *s == slot)
            .map(|(_, action)| action.clone())
    }
}

impl Render for RingView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Fixed Options+-style identity in both themes: floating black circle
        // buttons, white pill labels, no panel behind anything. The theme's
        // primary marks the aimed sector.
        let accent = cx.theme().primary;
        let (cx0, cy0) = (RING_W / 2., RING_H / 2.);

        let sectors = self.slots.iter().flat_map(|(slot, action)| {
            let angle = slot.angle_degrees().to_radians();
            let sin = angle.sin();
            let bx = cx0 + SECTOR_RADIUS * sin - SECTOR_BUTTON / 2.;
            let by = cy0 - SECTOR_RADIUS * angle.cos() - SECTOR_BUTTON / 2.;
            let aimed = self.aim == Aim::Sector(*slot);
            let slot_for_click = *slot;
            let button = div().absolute().left(px(bx)).top(px(by)).child(
                div()
                    .id(SharedStringId(slot.label()))
                    .size(px(SECTOR_BUTTON))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(if aimed {
                        accent
                    } else {
                        gpui::rgb(PLATE).into()
                    })
                    .shadow_md()
                    .child(
                        Icon::empty()
                            .path(action_icon_path(action))
                            .size_5()
                            .text_color(if aimed {
                                gpui::rgb(WHITE)
                            } else {
                                gpui::rgb(INK)
                            }),
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.defer(move |cx| {
                            fire(slot_for_click, cx);
                            close(cx);
                        });
                    })),
            );
            // The label pill floats just outside the button along its angle:
            // above the top sectors, beside the side ones (the Options+
            // layout). Anchored by direction so long labels grow outward.
            let anchor_r = SECTOR_RADIUS + SECTOR_BUTTON / 2. + LABEL_GAP;
            let ax = cx0 + anchor_r * sin;
            let ay = cy0 - anchor_r * angle.cos();
            let pill = div()
                .px_2()
                .py_0p5()
                .rounded_md()
                .bg(gpui::rgb(INK))
                .shadow_md()
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(gpui::rgb(PILL_TEXT))
                .when(aimed, |pill| pill.bg(accent).text_color(gpui::rgb(WHITE)))
                .child(tr!(action.label()));
            let row = div()
                .absolute()
                .top(px(ay - 11.))
                .left(px(0.))
                .w(px(RING_W))
                .flex();
            let label = if sin > 0.35 {
                row.justify_start().pl(px(ax)).child(pill)
            } else if sin < -0.35 {
                row.justify_end().pr(px(RING_W - ax)).child(pill)
            } else {
                row.justify_center().child(pill)
            };
            [button, label]
        });

        div().size_full().relative().children(sectors).child(
            // Centre ✕ — click (or a second pad tap in the dead zone) cancels.
            div()
                .id("ring-cancel")
                .absolute()
                .left(px(cx0 - CENTER_BUTTON / 2.))
                .top(px(cy0 - CENTER_BUTTON / 2.))
                .size(px(CENTER_BUTTON))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(if self.aim == Aim::Center {
                    gpui::rgb(CROSS_BG_AIMED)
                } else {
                    gpui::rgb(CROSS_BG)
                })
                .shadow_sm()
                .text_xs()
                .text_color(gpui::rgb(CROSS_TEXT))
                .child("✕")
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.defer(close);
                })),
        )
    }
}

/// Ad-hoc `ElementId` source for per-slot stateful divs.
#[derive(Clone)]
struct SharedStringId(&'static str);

impl From<SharedStringId> for gpui::ElementId {
    fn from(id: SharedStringId) -> Self {
        gpui::ElementId::Name(id.0.into())
    }
}

/// Raw Win32 plumbing for the overlay: the style bits gpui doesn't expose
/// (no-activate, topmost), show/hide without activation, physical-pixel
/// positioning, the global cursor, and the Esc / outside-click cancel
/// signals a focusless window can't observe as messages.
#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "raw Win32 window-style and cursor FFI — the pinned gpui backend exposes none of it"
)]
mod platform_win {
    use gpui::Point;
    use gpui::point;
    use windows_sys::Win32::Foundation::{POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    };
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_ESCAPE, VK_LBUTTON,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetCursorPos, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, SW_HIDE,
        SWP_NOACTIVATE, SetWindowLongPtrW, SetWindowPos, ShowWindow, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW,
    };

    /// Add the no-activate + toolwindow ex-style bits. Topmost is applied on
    /// every [`show_at`] (a style bit alone does not raise the window).
    pub fn apply_overlay_style(hwnd: isize) {
        // SAFETY: plain style read-modify-write on a window this process owns.
        unsafe {
            let hwnd = hwnd as _;
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(
                hwnd,
                GWL_EXSTYLE,
                ex | isize::from_ne_bytes(
                    ((WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW) as usize).to_ne_bytes(),
                ),
            );
        }
    }

    /// Physical-per-logical scale of the display the window is on.
    #[expect(
        clippy::cast_precision_loss,
        reason = "display DPI values are small integers — exact in f32"
    )]
    pub fn window_scale(hwnd: isize) -> f32 {
        // SAFETY: DPI read on a window this process owns.
        let dpi = unsafe { GetDpiForWindow(hwnd as _) };
        if dpi == 0 { 1. } else { dpi as f32 / 96. }
    }

    /// Centre the window (physical size `w`×`h`) at `cursor` (physical),
    /// clamped into the cursor's monitor **work area** so the ring never
    /// hangs off a screen edge, and show it topmost without activating it.
    /// Returns the window's actual centre — near an edge it differs from the
    /// cursor, and the sector hit-test must aim from where the ring really is.
    pub fn show_at(hwnd: isize, cursor: Point<f32>, w: f32, h: f32) -> Point<f32> {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "screen coordinates are far inside i32 range"
        )]
        let (cx, cy, w, h) = (cursor.x as i32, cursor.y as i32, w as i32, h as i32);
        let mut x = cx - w / 2;
        let mut y = cy - h / 2;
        // SAFETY: monitor lookup for the cursor's point, then position + show
        // of a window this process owns; SWP_NOACTIVATE keeps focus where the
        // user is working.
        unsafe {
            let monitor = MonitorFromPoint(POINT { x: cx, y: cy }, MONITOR_DEFAULTTONEAREST);
            let zero = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a fixed struct size is far below u32::MAX"
            )]
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                rcMonitor: zero,
                rcWork: zero,
                dwFlags: 0,
            };
            if GetMonitorInfoW(monitor, &raw mut info) != 0 {
                let work = info.rcWork;
                x = x.max(work.left).min(work.right - w);
                y = y.max(work.top).min(work.bottom - h);
            }
            SetWindowPos(
                hwnd as _,
                HWND_TOPMOST,
                x,
                y,
                w,
                h,
                SWP_NOACTIVATE | windows_sys::Win32::UI::WindowsAndMessaging::SWP_SHOWWINDOW,
            );
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "screen coordinates are far inside f32's exact-integer range"
        )]
        point((x + w / 2) as f32, (y + h / 2) as f32)
    }

    /// Disable the window's DWM "accent" effect. gpui's `Transparent`
    /// background enables `ACCENT_ENABLE_TRANSPARENTGRADIENT`, which paints a
    /// faint veil over the whole window rect; DirectComposition already
    /// composites the surface with per-pixel alpha, so the veil is pure
    /// artifact — the visible "box" behind the floating ring. This calls the
    /// same undocumented user32 API gpui itself uses, with the accent off.
    pub fn clear_accent(hwnd: isize) {
        #[repr(C)]
        struct AccentPolicy {
            state: u32,
            flags: u32,
            gradient_color: u32,
            animation_id: u32,
        }
        #[repr(C)]
        struct CompositionAttribData {
            attrib: u32,
            data: *mut core::ffi::c_void,
            size: u32,
        }
        const WCA_ACCENT_POLICY: u32 = 19;
        type SetWca =
            unsafe extern "system" fn(hwnd: isize, data: *mut CompositionAttribData) -> i32;

        // SAFETY: resolve the export from user32 (it is not in the import
        // library), then call it with a stack-valid attribute payload.
        unsafe {
            let user32: Vec<u16> = "user32.dll\0".encode_utf16().collect();
            let module = GetModuleHandleW(user32.as_ptr());
            if module.is_null() {
                return;
            }
            let Some(addr) =
                GetProcAddress(module, c"SetWindowCompositionAttribute".as_ptr().cast())
            else {
                return;
            };
            let set_wca: SetWca = core::mem::transmute(addr);
            let mut policy = AccentPolicy {
                state: 0, // ACCENT_DISABLED
                flags: 0,
                gradient_color: 0,
                animation_id: 0,
            };
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a fixed struct size is far below u32::MAX"
            )]
            let mut data = CompositionAttribData {
                attrib: WCA_ACCENT_POLICY,
                data: (&raw mut policy).cast(),
                size: size_of::<AccentPolicy>() as u32,
            };
            set_wca(hwnd, &raw mut data);
        }
    }

    /// Hide the window (kept alive for the next open).
    pub fn hide(hwnd: isize) {
        // SAFETY: hide of a window this process owns.
        unsafe {
            ShowWindow(hwnd as _, SW_HIDE);
        }
    }

    /// Global cursor position in physical pixels.
    pub fn cursor_pos() -> Option<Point<f32>> {
        let mut p = POINT { x: 0, y: 0 };
        // SAFETY: out-pointer to a stack POINT.
        #[expect(
            clippy::cast_precision_loss,
            reason = "screen coordinates are far inside f32's exact-integer range"
        )]
        if unsafe { GetCursorPos(&raw mut p) } != 0 {
            Some(point(p.x as f32, p.y as f32))
        } else {
            None
        }
    }

    /// Whether Esc is down right now (edge behaviour handled by the caller's
    /// close-once state machine — the ring is gone before a repeat matters).
    pub fn escape_pressed() -> bool {
        // SAFETY: stateless key query.
        unsafe { GetAsyncKeyState(i32::from(VK_ESCAPE)).cast_unsigned() & 0x8000 != 0 }
    }

    /// Whether the left button is down with the cursor outside the window —
    /// the "click outside cancels" signal, observed by polling because a
    /// no-activate window receives no messages for clicks elsewhere.
    pub fn clicked_outside(hwnd: isize) -> bool {
        // SAFETY: stateless key query + window-rect read on an owned window.
        unsafe {
            if GetAsyncKeyState(i32::from(VK_LBUTTON)).cast_unsigned() & 0x8000 == 0 {
                return false;
            }
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if GetWindowRect(hwnd as _, &raw mut rect) == 0 {
                return false;
            }
            let mut p = POINT { x: 0, y: 0 };
            if GetCursorPos(&raw mut p) == 0 {
                return false;
            }
            p.x < rect.left || p.x > rect.right || p.y < rect.top || p.y > rect.bottom
        }
    }
}

/// Non-Windows stubs: the ring is captured and bindable everywhere, but the
/// overlay window itself is Windows-first (macOS/Linux land with their own
/// platform plumbing).
#[cfg(not(target_os = "windows"))]
mod platform_win {
    use gpui::Point;

    pub fn apply_overlay_style(_hwnd: isize) {}
    pub fn window_scale(_hwnd: isize) -> f32 {
        1.
    }
    pub fn show_at(_hwnd: isize, cursor: Point<f32>, _w: f32, _h: f32) -> Point<f32> {
        cursor
    }
    pub fn clear_accent(_hwnd: isize) {}
    pub fn hide(_hwnd: isize) {}
    pub fn cursor_pos() -> Option<Point<f32>> {
        None
    }
    pub fn escape_pressed() -> bool {
        false
    }
    pub fn clicked_outside(_hwnd: isize) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sector(center: Point<f32>, dx: f32, dy: f32) -> Aim {
        aim_for(center, point(center.x + dx, center.y + dy), 1.)
    }

    #[test]
    fn dead_zone_aims_center() {
        let c = point(500., 500.);
        assert_eq!(sector(c, 0., 0.), Aim::Center);
        assert_eq!(sector(c, 30., -20.), Aim::Center, "inside the dead zone");
    }

    #[test]
    fn cardinal_directions_pick_their_slots() {
        let c = point(500., 500.);
        assert_eq!(sector(c, 0., -200.), Aim::Sector(RingSlot::North));
        assert_eq!(sector(c, 200., 0.), Aim::Sector(RingSlot::East));
        assert_eq!(sector(c, 0., 200.), Aim::Sector(RingSlot::South));
        assert_eq!(sector(c, -200., 0.), Aim::Sector(RingSlot::West));
    }

    #[test]
    fn diagonals_and_sector_boundaries_route_clockwise() {
        let c = point(0., 0.);
        assert_eq!(sector(c, 100., -100.), Aim::Sector(RingSlot::NorthEast));
        assert_eq!(sector(c, 100., 100.), Aim::Sector(RingSlot::SouthEast));
        assert_eq!(sector(c, -100., 100.), Aim::Sector(RingSlot::SouthWest));
        assert_eq!(sector(c, -100., -100.), Aim::Sector(RingSlot::NorthWest));
        // 22.4° off vertical still rounds to North; 22.6° tips into NorthEast.
        let west_of_boundary = (22.4_f32).to_radians();
        let east_of_boundary = (22.6_f32).to_radians();
        assert_eq!(
            sector(
                c,
                200. * west_of_boundary.sin(),
                -200. * west_of_boundary.cos()
            ),
            Aim::Sector(RingSlot::North)
        );
        assert_eq!(
            sector(
                c,
                200. * east_of_boundary.sin(),
                -200. * east_of_boundary.cos()
            ),
            Aim::Sector(RingSlot::NorthEast)
        );
    }

    #[test]
    fn scale_grows_the_dead_zone() {
        let c = point(0., 0.);
        // 60 px out: a sector at 1×, still the dead zone at 2×.
        assert_eq!(
            aim_for(c, point(0., -60.), 1.),
            Aim::Sector(RingSlot::North)
        );
        assert_eq!(aim_for(c, point(0., -60.), 2.), Aim::Center);
    }
}
