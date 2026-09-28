//! The "Add device" window — drives a wireless pairing session.
//!
//! Pairing runs in the **agent** (it owns device I/O, so it opens the receiver,
//! not the GUI). This window is a thin state machine that talks to the agent
//! over IPC:
//!
//! - The buttons send [`StartPairing`] / [`PairDevice`] / [`CancelPairing`]
//!   through the agent IPC client.
//! - [`PairingUi`] — the latest session state, taken from the agent's observed
//!   state by the runtime via [`apply_state`], or a refusal the agent never
//!   turned into a session via [`apply_undeliverable`]. The view observes it
//!   and repaints on change.
//!
//! Bolt is interactive (discover → pick → enter a passkey on the device);
//! Unifying just opens a lock and waits for the next device to link, so it
//! jumps straight from *searching* to *paired*.

use gpui::{
    App, Context, FocusHandle, FontWeight, Global, InteractiveElement, IntoElement,
    ParentElement as _, Render, SharedString, Size, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, prelude::FluentBuilder as _, px, svg,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use openlogi_core::device::ReceiverInfo;
use openlogi_core::hid::{Click, PasskeyMethod, ReceiverSelector, find_receiver};
use openlogi_ipc::{FoundDevice, PairingFailure, PairingPhase};

use crate::app::menu::{CloseWindow, Minimize, Zoom};
use crate::services::ipc::{CancelPairing, Command, PairDevice, StartPairing};
use crate::state::{AppState, StateEvent};
use crate::ui::theme::{self, Palette, Typography as _};
use crate::windows::{self, AuxWindow};

/// The pairing flow as the window renders it: the agent's [`PairingPhase`]
/// plus [`Self::Idle`] for no session.
#[derive(Clone, Default, PartialEq, Eq)]
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
    /// The session ended without pairing.
    Failed(PairingFailure),
    /// Waiting for the agent to restore the receiver and release its session.
    Cancelling,
}

impl Global for PairingUi {}

/// The user's pairing target outlives the auxiliary window, just like the
/// agent's session. Closing a window must not discard the target for Retry.
#[derive(Default)]
struct PairingSelection(Option<ReceiverInfo>);

impl Global for PairingSelection {}

/// Open the Add Device window. The user chooses a receiver before discovery
/// starts; re-opening an active session just focuses the existing window.
pub fn open(cx: &mut App) {
    windows::open_or_focus(
        |reg| &mut reg.add_device,
        window_title(),
        Size::new(px(520.), px(460.)),
        AddDeviceView::new,
        cx,
    );
}

/// The window's native title — one definition for open and the live-language
/// retitle ([`windows::retitle_open`]), so the two cannot drift.
pub(crate) fn window_title() -> SharedString {
    tr!("pairing.add_device")
}

/// Show the agent's pairing session. `None` is no session — including after a
/// cancel, and after an agent restart, which is why a window left mid-flow no
/// longer needs a terminal event synthesized on its behalf.
///
/// The accumulation this used to do (collecting discovered devices out of an
/// event stream) belongs to the agent, which is the side that knows what it has
/// discovered; nothing is folded here any more.
pub fn apply_state(cx: &mut App, phase: Option<PairingPhase>) {
    if matches!(cx.try_global::<PairingUi>(), Some(PairingUi::Cancelling)) {
        // Only the command acknowledgement proves cancellation finished.
        return;
    }
    let next = match phase {
        None => PairingUi::Idle,
        Some(PairingPhase::Searching) => PairingUi::Searching,
        Some(PairingPhase::Found(devices)) => PairingUi::Found(devices),
        Some(PairingPhase::Pairing) => PairingUi::Pairing,
        Some(PairingPhase::Passkey(method)) => PairingUi::Passkey(method),
        Some(PairingPhase::Paired { slot }) => PairingUi::Paired { slot },
        Some(PairingPhase::Failed(failure)) => PairingUi::Failed(failure),
    };
    if cx.try_global::<PairingUi>() == Some(&next) {
        return;
    }
    cx.set_global(next);
}

/// Report a pairing command the client could not deliver. No session will ever
/// appear to explain the silence, so the window has to be told directly.
pub fn apply_undeliverable(cx: &mut App, failure: PairingFailure) {
    cx.set_global(PairingUi::Failed(failure));
}

/// The agent has finished cancellation, including when no session existed.
pub fn apply_cancelled(cx: &mut App) {
    cx.set_global(PairingUi::Idle);
}

fn pairing_failure_text(failure: &PairingFailure) -> String {
    match failure {
        PairingFailure::Hid { message } => {
            tr!("pairing.hid_transport_error", message => message.clone()).to_string()
        }
        PairingFailure::ReceiverNotFound => tr!("pairing.pairing_receiver_not_found").to_string(),
        PairingFailure::Register { message } => {
            tr!("pairing.receiver_register_error", message => message.clone()).to_string()
        }
        PairingFailure::Timeout => tr!("pairing.pairing_timed_out").to_string(),
        PairingFailure::Device { code } => tr!(
            "pairing.receiver_pairing_error",
            code => format!("0x{code:02x}"),
        )
        .to_string(),
        PairingFailure::Cancelled => tr!("pairing.pairing_was_cancelled").to_string(),
        PairingFailure::ReceiverBusy => {
            tr!("pairing.the_receiver_is_busy_try_pairing_again").to_string()
        }
        PairingFailure::WatcherUnavailable => tr!("pairing.pairing_agent_not_ready").to_string(),
        PairingFailure::AgentRestarted => tr!("agent.agent_restarted_during_pairing").to_string(),
        PairingFailure::ReceiverAccessUnavailable => {
            tr!("pairing.pairing_receiver_access_unrecorded").to_string()
        }
        PairingFailure::AlreadyActive => {
            tr!("pairing.a_pairing_session_is_already_active").to_string()
        }
        PairingFailure::UnknownDevice => {
            tr!("pairing.pairing_device_no_longer_available").to_string()
        }
        PairingFailure::NoActiveSession => tr!("pairing.no_pairing_session_is_active").to_string(),
    }
}

fn send(cx: &App, command: impl Into<Command>) {
    if let Some(state) = AppState::try_global(cx) {
        let _ = state.read(cx).ipc_sender().send(command.into());
    }
}

fn receiver_selector(receiver: &ReceiverInfo) -> Option<ReceiverSelector> {
    find_receiver(receiver.vendor_id, receiver.product_id)?;
    let uid = receiver.unique_id.as_ref().filter(|uid| !uid.is_empty())?;
    Some(ReceiverSelector::ReceiverUid {
        product_id: receiver.product_id,
        uid: uid.clone(),
    })
}

fn start_search(cx: &mut App, selector: ReceiverSelector) {
    cx.set_global(PairingUi::Searching);
    send(cx, StartPairing { selector });
}

fn cancel_search(cx: &mut App) {
    cx.set_global(PairingUi::Cancelling);
    send(cx, CancelPairing);
}

/// Standalone Add Device window root view.
pub struct AddDeviceView {
    focus_handle: FocusHandle,
    appearance_obs: Option<Subscription>,
    #[expect(dead_code, reason = "held to keep the PairingUi observer alive")]
    state_obs: Subscription,
    #[expect(
        dead_code,
        reason = "held to repaint when the receiver inventory changes"
    )]
    inventory_obs: Subscription,
}

impl AddDeviceView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let state_obs = cx.observe_global::<PairingUi>(|_, cx| cx.notify());
        let inventory_obs = AppState::repaint_on(cx, |event| {
            matches!(
                event,
                StateEvent::InventoryChanged | StateEvent::AgentChanged
            )
        });
        Self {
            focus_handle,
            appearance_obs: None,
            state_obs,
            inventory_obs,
        }
    }

    fn receiver_picker(pal: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let receivers: Vec<_> = AppState::try_read(cx)
            .filter(|state| state.agent_status().is_some())
            .into_iter()
            .flat_map(AppState::last_inventory)
            .map(|inventory| &inventory.receiver)
            .filter(|receiver| find_receiver(receiver.vendor_id, receiver.product_id).is_some())
            .cloned()
            .collect();
        let mut col = v_flex()
            .w_full()
            .gap_3()
            .child(status_line(tr!("pairing.choose_receiver")))
            .child(hint(tr!("pairing.choose_receiver_hint"), pal));
        if receivers.is_empty() {
            return col.child(hint(tr!("pairing.connect_receiver"), pal));
        }
        for receiver in receivers {
            let Some(selector) = receiver_selector(&receiver) else {
                col = col.child(hint(
                    tr!("pairing.receiver_unavailable", name => receiver.name.clone()),
                    pal,
                ));
                continue;
            };
            let id = receiver_element_id(&receiver);
            let label = receiver_label(&receiver);
            col = col.child(
                BaseButton::new(SharedString::from(id.clone()))
                    .debug_selector(move || id.clone())
                    .accessibility_label(label.clone())
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_start()
                    .px_4()
                    .py_3()
                    .rounded(pal.control_radius)
                    .border_1()
                    .border_color(pal.border)
                    .bg(pal.control)
                    .hover(|s| s.bg(pal.control_hover))
                    .focus_visible(|s| s.bg(pal.control_hover))
                    .child(div().text_body().child(label))
                    .on_click(move |_, _, cx| {
                        cx.set_global(PairingSelection(Some(receiver.clone())));
                        start_search(cx, selector.clone());
                    }),
            );
        }
        col
    }
}

fn receiver_element_id(receiver: &ReceiverInfo) -> String {
    format!(
        "pairing-receiver-{:04x}-{}",
        receiver.product_id,
        receiver.unique_id.as_deref().unwrap_or_default()
    )
}

fn receiver_label(receiver: &ReceiverInfo) -> SharedString {
    // The suffix distinguishes otherwise identical receivers without exposing a
    // full hardware identifier in the normal pairing UI.
    let suffix: String = receiver
        .unique_id
        .as_deref()
        .unwrap_or_default()
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{} · {suffix}", receiver.name).into()
}

impl AuxWindow for AddDeviceView {
    fn set_appearance_obs(&mut self, sub: Subscription) {
        self.appearance_obs = Some(sub);
    }
}

impl Render for AddDeviceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        theme::apply_ui_scale(window, cx);
        let pal = theme::palette(cx);
        let state = cx.try_global::<PairingUi>().cloned().unwrap_or_default();
        let body = if state == PairingUi::Idle {
            Self::receiver_picker(pal, cx)
        } else {
            let selected = cx
                .try_global::<PairingSelection>()
                .and_then(|s| s.0.as_ref());
            let target = selected.and_then(receiver_selector);
            let mut body = v_flex().w_full().gap_3();
            if let Some(receiver) = selected {
                body = body.child(hint(receiver_label(receiver), pal));
            }
            body.child(pairing_body(state, pal, target))
        };

        v_flex()
            .size_full()
            .bg(pal.page)
            .text_color(pal.text_primary)
            .track_focus(&self.focus_handle)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            // Linux only: a client-side titlebar at the top of the window; the
            // padded content sits in the flex-column below it. macOS / Windows
            // keep their native titlebar.
            .when(cfg!(target_os = "linux"), |this| {
                this.child(windows::aux_title_bar(tr!("pairing.add_device"), cx))
            })
            .child(
                v_flex()
                    .flex_1()
                    .w_full()
                    .p_6()
                    .gap_5()
                    .child(
                        div()
                            .text_heading()
                            .child(tr!("pairing.add_device")),
                    )
                    .min_h_0()
                    .child(div().id("pairing-content").overflow_y_scroll().child(body)),
            )
    }
}

fn pairing_body(
    state: PairingUi,
    pal: Palette,
    target: Option<ReceiverSelector>,
) -> impl IntoElement {
    let mut col = v_flex().w_full().flex_1().gap_4();
    match state {
        PairingUi::Idle => {}
        PairingUi::Cancelling => {
            col = col.child(status_line(tr!("pairing.cancelling")));
        }
        PairingUi::Searching => {
            col = col
                .child(status_line(tr!("pairing.searching_for_devices")))
                .child(hint(tr!("pairing.pairing_search_hint"), pal))
                .child(cancel_button());
        }
        PairingUi::Found(devices) => {
            col = col.child(status_line(tr!("pairing.searching_for_devices")));
            if devices.is_empty() {
                col = col.child(hint(tr!("pairing.no_devices_found_yet"), pal));
            } else {
                col = col.child(hint(tr!("pairing.select_a_device_to_pair"), pal));
                for device in &devices {
                    col = col.child(device_row(device, pal));
                }
            }
            col = col.child(cancel_button());
        }
        PairingUi::Pairing => {
            col = col
                .child(status_line(tr!("pairing.pairing")))
                .child(hint(
                    tr!("pairing.follow_the_instructions_on_your_device"),
                    pal,
                ))
                .child(cancel_button());
        }
        PairingUi::Passkey(method) => {
            col = col.child(passkey_panel(&method, pal));
            col = col.child(cancel_button());
        }
        PairingUi::Paired { slot } => {
            col = col
                .child(
                    div()
                        .text_color(pal.text_primary)
                        .font_weight(FontWeight::MEDIUM)
                        .child(tr!("pairing.device_paired")),
                )
                .child(hint(
                    tr!("pairing.paired_receiver_slot", slot => slot.to_string()),
                    pal,
                ))
                .child(
                    action_button("ad-done", tr!("common.done"), false)
                        .on_click(|_, _, cx| cancel_search(cx)),
                );
        }
        PairingUi::Failed(failure) => {
            col = col
                .child(
                    div()
                        .text_color(pal.text_primary)
                        .font_weight(FontWeight::MEDIUM)
                        .child(tr!("pairing.pairing_failed")),
                )
                .child(hint(pairing_failure_text(&failure), pal))
                .when(
                    matches!(failure, PairingFailure::ReceiverNotFound),
                    |this| this.child(hint(tr!("device.device_connection_help"), pal)),
                )
                .when_some(target, |this, selector| {
                    this.child(
                        action_button("ad-retry", tr!("common.try_again"), true)
                            .debug_selector(|| "pairing-retry".to_string())
                            .on_click(move |_, _, cx| start_search(cx, selector.clone())),
                    )
                })
                .child(
                    action_button("ad-change-receiver", tr!("pairing.change_receiver"), false)
                        .debug_selector(|| "pairing-change-receiver".to_string())
                        .on_click(|_, _, cx| cancel_search(cx)),
                );
        }
    }
    col
}

/// A discovered-device row; clicking it pairs with that device.
fn device_row(device: &FoundDevice, pal: Palette) -> impl IntoElement {
    let address = device.address;
    let address_id = u64::from_be_bytes([
        0, 0, address[0], address[1], address[2], address[3], address[4], address[5],
    ]);
    let name = SharedString::from(device.name.clone());
    BaseButton::new(("found-device", address_id))
        .accessibility_label(name.clone())
        .w_full()
        .flex()
        .items_center()
        .justify_start()
        .px_4()
        .py_3()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        .cursor_pointer()
        .bg(pal.control)
        .hover(|s| s.bg(pal.control_hover))
        .focus_visible(|s| s.bg(pal.control_hover))
        .child(div().text_body().child(name))
        .on_click(move |_, _, cx| send(cx, PairDevice { address }))
}

/// The passkey-entry instructions panel.
fn passkey_panel(method: &PasskeyMethod, pal: Palette) -> impl IntoElement {
    let mut col = v_flex().w_full().gap_3();
    match method {
        PasskeyMethod::Keyboard(digits) => {
            col = col
                .child(status_line(tr!(
                    "pairing.keyboard_pairing_passkey_instructions"
                )))
                .child(div().text_title().child(SharedString::from(digits.clone())));
        }
        PasskeyMethod::Pointer { clicks, .. } => {
            col = col
                .child(status_line(tr!(
                    "pairing.mouse_pairing_passkey_instructions"
                )))
                .child(
                    h_flex()
                        .id("passkey-sequence")
                        // The icons carry no text of their own, so the order is
                        // spelled out once here rather than left to assistive
                        // tech as a row of unlabelled images.
                        .aria_label(spoken_click_sequence(clicks))
                        .gap_2()
                        .children(clicks.iter().enumerate().map(|(step, click)| {
                            v_flex()
                                .items_center()
                                .gap_0p5()
                                .child(svg().path(click_icon(*click)).size_6().flex_none())
                                .child(
                                    div()
                                        .text_caption()
                                        .text_color(pal.text_muted)
                                        .child((step + 1).to_string()),
                                )
                        })),
                );
        }
    }
    col
}

/// The mouse body with the button this step wants filled in.
fn click_icon(click: Click) -> &'static str {
    match click {
        Click::Left => "action-icons/mouse-left.svg",
        Click::Right => "action-icons/mouse-right.svg",
    }
}

/// The click sequence as an ordered sentence, for the accessibility tree.
fn spoken_click_sequence(clicks: &[Click]) -> String {
    clicks
        .iter()
        .enumerate()
        .map(|(step, click)| {
            let label = match click {
                Click::Left => tr!("actions.left_click"),
                Click::Right => tr!("actions.right_click"),
            };
            format!("{}. {label}", step + 1)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn status_line(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .text_body()
        .font_weight(FontWeight::MEDIUM)
        .child(text.into())
}

fn hint(text: impl Into<SharedString>, pal: Palette) -> impl IntoElement {
    div()
        .text_caption()
        .text_color(pal.text_muted)
        .child(text.into())
}

/// A styled button. `primary` paints it accent-filled; otherwise it's the
/// neutral default. The caller attaches `.on_click`.
fn action_button(id: &'static str, label: impl Into<SharedString>, primary: bool) -> Button {
    let button = Button::new(id).label(label);
    if primary { button.primary() } else { button }
}

fn cancel_button() -> impl IntoElement {
    action_button("ad-cancel", tr!("common.cancel"), false).on_click(|_, _, cx| cancel_search(cx))
}

#[cfg(test)]
mod tests;
