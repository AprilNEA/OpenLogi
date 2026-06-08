//! The "Add device" window — drives a wireless pairing session.
//!
//! Pairing runs in the **agent** (it owns device I/O, so it opens the receiver,
//! not the GUI). This window is a thin state machine that talks to the agent
//! over IPC:
//!
//! - The buttons send [`Command::StartPairing`] / [`Command::PairDevice`] /
//!   [`Command::CancelPairing`] through the agent IPC client.
//! - [`PairingUi`] — the latest session state, updated from the agent's pairing
//!   long-poll ([`crate::ipc_client::IpcClient::pairing`]) in [`crate::main`]'s
//!   loop via [`apply_update`]. The view observes it and repaints on change.
//!
//! Bolt is interactive (discover → pick → enter a passkey on the device);
//! Unifying just opens a lock and waits for the next device to link, so it
//! jumps straight from *searching* to *paired*.

use gpui::{
    App, Context, FontWeight, Global, InteractiveElement, IntoElement, ParentElement as _, Render,
    SharedString, Size, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
    px, rgb,
};
use gpui_component::v_flex;
use openlogi_agent_core::ipc::{FoundDevice, PairingUpdate};
use openlogi_hid::{Click, PasskeyMethod, ReceiverSelector};

use crate::app_menu::{CloseWindow, Minimize, Zoom};
use crate::ipc_client::Command;
use crate::state::AppState;
use crate::theme::{self, Palette};
use crate::windows::{self, AuxWindow};

/// The pairing flow's current UI state. Mirrors the [`PairingUpdate`] stream.
#[derive(Clone, Default)]
pub enum PairingUi {
    /// No session in flight (initial, or after Done / dismissing a failure).
    #[default]
    Idle,
    /// Discovery (Bolt) or the pairing lock (Unifying) is open.
    Searching,
    /// Bolt: devices discovered so far, awaiting the user's pick.
    Found(Vec<FoundDevice>),
    /// A device was picked; waiting for the receiver's next step.
    Pairing,
    /// Bolt: the device asks the user to enter a passkey.
    Passkey(PasskeyMethod),
    /// A device paired into `slot`.
    Paired { slot: u8 },
    /// The session ended without pairing; carries a human-readable detail.
    Failed(String),
}

impl Global for PairingUi {}

/// Open the Add Device window, starting a fresh search unless one is already
/// in flight (re-opening just focuses the existing window).
pub fn open(cx: &mut App) {
    let active = matches!(
        cx.try_global::<PairingUi>(),
        Some(
            PairingUi::Searching | PairingUi::Found(_) | PairingUi::Pairing | PairingUi::Passkey(_)
        )
    );
    if !active {
        start_search(cx);
    }
    windows::open_or_focus(
        |reg| &mut reg.add_device,
        tr!("Add Device"),
        Size::new(px(520.), px(460.)),
        AddDeviceView::new,
        cx,
    );
}

/// Fold a pairing update into [`PairingUi`]. Called from the GPUI event loop as
/// the agent's pairing long-poll delivers events.
pub fn apply_update(cx: &mut App, update: PairingUpdate) {
    let current = cx.try_global::<PairingUi>().cloned().unwrap_or_default();
    let next = match update {
        PairingUpdate::Searching => PairingUi::Searching,
        PairingUpdate::DeviceFound(device) => {
            let mut devices = match current {
                PairingUi::Found(devices) => devices,
                _ => Vec::new(),
            };
            if !devices.iter().any(|d| d.address == device.address) {
                devices.push(device);
            }
            PairingUi::Found(devices)
        }
        PairingUpdate::Passkey(method) => PairingUi::Passkey(method),
        PairingUpdate::Paired { slot } => PairingUi::Paired { slot },
        PairingUpdate::Failed(detail) => PairingUi::Failed(detail),
    };
    cx.set_global(next);
}

fn send(cx: &App, command: Command) {
    if let Some(state) = cx.try_global::<AppState>() {
        let _ = state.ipc_sender().send(command);
    }
}

fn start_search(cx: &mut App) {
    cx.set_global(PairingUi::Searching);
    send(cx, Command::StartPairing(ReceiverSelector::First));
}

/// Standalone Add Device window root view.
pub struct AddDeviceView {
    #[allow(dead_code, reason = "held to keep the appearance observer alive")]
    appearance_obs: Option<Subscription>,
    #[allow(dead_code, reason = "held to keep the PairingUi observer alive")]
    state_obs: Subscription,
}

impl AddDeviceView {
    fn new(_: &mut Window, cx: &mut Context<Self>) -> Self {
        let state_obs = cx.observe_global::<PairingUi>(|_, cx| cx.notify());
        Self {
            appearance_obs: None,
            state_obs,
        }
    }
}

impl AuxWindow for AddDeviceView {
    fn set_appearance_obs(&mut self, sub: Subscription) {
        self.appearance_obs = Some(sub);
    }
}

impl Render for AddDeviceView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let state = cx.try_global::<PairingUi>().cloned().unwrap_or_default();

        v_flex()
            .size_full()
            .bg(pal.bg)
            .text_color(pal.text_primary)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            .p_6()
            .gap_5()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr!("Add Device")),
            )
            .child(body(&state, pal))
    }
}

/// The state-dependent body of the window.
fn body(state: &PairingUi, pal: Palette) -> impl IntoElement {
    let mut col = v_flex().w_full().flex_1().gap_4();
    match state {
        PairingUi::Idle => {
            col = col
                .child(hint(
                    tr!("Put the device in pairing mode, then start searching."),
                    pal,
                ))
                .child(
                    action_button("ad-search", tr!("Search for devices"), pal, true)
                        .on_click(|_, _, cx| start_search(cx)),
                );
        }
        PairingUi::Searching => {
            col = col
                .child(status_line(tr!("Searching for devices…"), pal))
                .child(hint(
                    tr!("Make sure the device is on and in pairing mode."),
                    pal,
                ))
                .child(cancel_button(pal));
        }
        PairingUi::Found(devices) => {
            col = col.child(status_line(tr!("Searching for devices…"), pal));
            if devices.is_empty() {
                col = col.child(hint(tr!("No devices found yet…"), pal));
            } else {
                col = col.child(hint(tr!("Select a device to pair:"), pal));
                for (idx, device) in devices.iter().enumerate() {
                    col = col.child(device_row(idx, device, pal));
                }
            }
            col = col.child(cancel_button(pal));
        }
        PairingUi::Pairing => {
            col = col
                .child(status_line(tr!("Pairing…"), pal))
                .child(hint(tr!("Follow the instructions on your device."), pal))
                .child(cancel_button(pal));
        }
        PairingUi::Passkey(method) => {
            col = col.child(passkey_panel(method, pal));
            col = col.child(cancel_button(pal));
        }
        PairingUi::Paired { slot } => {
            col = col
                .child(
                    div()
                        .text_color(rgb(theme::STATUS_CONNECTED))
                        .font_weight(FontWeight::MEDIUM)
                        .child(tr!("Device paired")),
                )
                .child(hint(
                    tr!("Paired to slot %{slot}.", slot => (*slot).to_string()),
                    pal,
                ))
                .child(
                    action_button("ad-done", tr!("Done"), pal, false)
                        .on_click(|_, _, cx| cx.set_global(PairingUi::Idle)),
                );
        }
        PairingUi::Failed(detail) => {
            col = col
                .child(
                    div()
                        .text_color(rgb(theme::STATUS_CONNECTING))
                        .font_weight(FontWeight::MEDIUM)
                        .child(tr!("Pairing failed")),
                )
                .child(hint(SharedString::from(detail.clone()), pal))
                .child(
                    action_button("ad-retry", tr!("Try again"), pal, true)
                        .on_click(|_, _, cx| start_search(cx)),
                );
        }
    }
    col
}

/// A discovered-device row; clicking it pairs with that device.
fn device_row(idx: usize, device: &FoundDevice, pal: Palette) -> impl IntoElement {
    let address = device.address;
    div()
        .id(("found-device", idx))
        .w_full()
        .px_4()
        .py_3()
        .rounded_md()
        .border_1()
        .border_color(pal.border)
        .cursor_pointer()
        .hover(|s| s.bg(pal.surface_hover))
        .child(
            div()
                .text_sm()
                .child(SharedString::from(device.name.clone())),
        )
        .on_click(move |_, _, cx| {
            send(cx, Command::PairDevice(address));
            cx.set_global(PairingUi::Pairing);
        })
}

/// The passkey-entry instructions panel.
fn passkey_panel(method: &PasskeyMethod, pal: Palette) -> impl IntoElement {
    let mut col = v_flex().w_full().gap_3();
    match method {
        PasskeyMethod::Keyboard(digits) => {
            col = col
                .child(status_line(
                    tr!("Type this passkey on the new keyboard, then press Enter:"),
                    pal,
                ))
                .child(
                    div()
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from(digits.clone())),
                );
        }
        PasskeyMethod::Pointer { clicks, .. } => {
            let sequence: String = clicks
                .iter()
                .map(|c| match c {
                    Click::Left => "←",
                    Click::Right => "→",
                })
                .collect::<Vec<_>>()
                .join(" ");
            col = col
                .child(status_line(
                    tr!("On the new mouse, click in this order, then press both buttons together:"),
                    pal,
                ))
                .child(
                    div()
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from(sequence)),
                );
        }
    }
    col
}

fn status_line(text: impl Into<SharedString>, _pal: Palette) -> impl IntoElement {
    div()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .child(text.into())
}

fn hint(text: impl Into<SharedString>, pal: Palette) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(pal.text_muted)
        .child(text.into())
}

/// A styled button. `primary` paints it accent-filled; otherwise it's outlined.
/// The caller attaches `.on_click`.
fn action_button(
    id: &'static str,
    label: impl Into<SharedString>,
    pal: Palette,
    primary: bool,
) -> gpui::Stateful<gpui::Div> {
    let base = div()
        .id(id)
        .px_4()
        .py_2()
        .rounded_md()
        .cursor_pointer()
        .child(label.into());
    if primary {
        base.bg(rgb(theme::ACCENT_BLUE))
            .text_color(rgb(0x00ff_ffff))
            .font_weight(FontWeight::MEDIUM)
    } else {
        base.border_1()
            .border_color(pal.border)
            .hover(|s| s.bg(pal.surface_hover))
    }
}

fn cancel_button(pal: Palette) -> impl IntoElement {
    action_button("ad-cancel", tr!("Cancel"), pal, false).on_click(|_, _, cx| {
        send(cx, Command::CancelPairing);
        cx.set_global(PairingUi::Idle);
    })
}
