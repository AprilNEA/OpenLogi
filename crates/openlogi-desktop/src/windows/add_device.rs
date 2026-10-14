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
    ParentElement as _, Render, RenderOnce, SharedString, Size, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, prelude::FluentBuilder as _, px, svg,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use openlogi_core::hid::{Click, PasskeyMethod, ReceiverSelector};
use openlogi_ipc::{FoundDevice, PairingFailure, PairingPhase};

use crate::app::menu::{CloseWindow, Minimize, Zoom};
use crate::services::ipc::{CancelPairing, Command, PairDevice, StartPairing};
use crate::state::AppState;
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

pub(crate) fn pairing_failure_text(failure: &PairingFailure) -> String {
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
        PairingFailure::ReceiverFull => tr!("pairing.receiver_full").to_string(),
    }
}

fn send(cx: &App, command: impl Into<Command>) {
    if let Some(state) = AppState::try_global(cx) {
        let _ = state.read(cx).ipc_sender().send(command.into());
    }
}

fn start_search(cx: &mut App) {
    send(
        cx,
        StartPairing {
            selector: ReceiverSelector::First,
        },
    );
}

/// Standalone Add Device window root view.
pub struct AddDeviceView {
    focus_handle: FocusHandle,
    appearance_obs: Option<Subscription>,
    #[expect(dead_code, reason = "held to keep the PairingUi observer alive")]
    state_obs: Subscription,
}

impl AddDeviceView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let state_obs = cx.observe_global::<PairingUi>(|_, cx| cx.notify());
        Self {
            focus_handle,
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        theme::apply_ui_scale(window, cx);
        let pal = theme::palette(cx);
        let state = cx.try_global::<PairingUi>().cloned().unwrap_or_default();

        v_flex()
            .size_full()
            .bg(pal.page)
            .text_color(pal.text_primary)
            .track_focus(&self.focus_handle)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            // Only where the compositor left the chrome to us; the padded
            // content sits in the flex-column below it.
            .when(windows::needs_client_titlebar(window), |this| {
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
                    .child(AddDeviceBody { state }),
            )
    }
}

/// The state-dependent body owns theme resolution for the complete pairing flow.
#[derive(IntoElement)]
struct AddDeviceBody {
    state: PairingUi,
}

impl RenderOnce for AddDeviceBody {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        pairing_body(self.state, pal)
    }
}

fn pairing_body(state: PairingUi, pal: Palette) -> impl IntoElement {
    let mut col = v_flex().w_full().flex_1().gap_4();
    match state {
        PairingUi::Idle => {
            col = col
                .child(hint(tr!("pairing.pairing_mode_description"), pal))
                .child(
                    action_button("ad-search", tr!("pairing.search_for_devices"), true)
                        .on_click(|_, _, cx| start_search(cx)),
                );
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
                        .on_click(|_, _, cx| send(cx, CancelPairing)),
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
                .child(
                    action_button("ad-retry", tr!("common.try_again"), true)
                        .on_click(|_, _, cx| start_search(cx)),
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
                        .debug_selector(|| "passkey-sequence".into())
                        // The icons carry no text of their own, so the order is
                        // spelled out once here rather than left to assistive
                        // tech as a row of unlabelled images.
                        .aria_label(spoken_click_sequence(clicks))
                        .gap_2()
                        .children(clicks.iter().enumerate().map(|(step, click)| {
                            v_flex()
                                .items_center()
                                .gap_0p5()
                                .child(click_icon_svg(*click, pal))
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

/// Click glyph with explicit text color so GPUI's SVG renderer paints its path.
fn click_icon_svg(click: Click, pal: Palette) -> gpui::Svg {
    svg()
        .path(click_icon(click))
        .size_6()
        .flex_none()
        .text_color(pal.text_primary)
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
    action_button("ad-cancel", tr!("common.cancel"), false)
        .on_click(|_, _, cx| send(cx, CancelPairing))
}

#[cfg(test)]
mod tests {
    use gpui::{AssetSource, TestAppContext};
    use openlogi_ui::action_icons::ActionIcons;

    use super::*;

    #[test]
    fn mouse_pairing_click_icons_are_embedded() {
        for click in [Click::Left, Click::Right] {
            let path = click_icon(click);
            let loaded = ActionIcons.load(path);
            assert!(
                matches!(loaded, Ok(Some(_))),
                "missing embedded asset for {path}"
            );
            let bytes = loaded.unwrap().unwrap();
            let content = std::str::from_utf8(&bytes).expect("valid utf-8 svg");
            assert!(content.contains("<svg"), "asset {path} should be an SVG");
        }
    }

    #[test]
    fn mouse_pairing_click_icon_resolves_text_color() {
        let text_primary = gpui::hsla(0.5, 0.5, 0.5, 1.0);
        let pal = Palette {
            page: gpui::hsla(0., 0., 0., 1.),
            panel: gpui::hsla(0., 0., 0., 1.),
            control: gpui::hsla(0., 0., 0., 1.),
            control_hover: gpui::hsla(0., 0., 0., 1.),
            muted: gpui::hsla(0., 0., 0., 1.),
            border: gpui::hsla(0., 0., 0., 1.),
            text_primary,
            text_muted: gpui::hsla(0., 0., 0., 1.),
            card_radius: gpui::px(8.),
            control_radius: gpui::px(4.),
        };
        for click in [Click::Left, Click::Right] {
            let mut icon = click_icon_svg(click, pal);
            assert_eq!(
                icon.style().text.color,
                Some(text_primary),
                "click icon SVG must resolve text color so GPUI paints its path"
            );
        }
    }

    #[test]
    fn spoken_click_sequence_formats_correctly() {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let clicks = [Click::Left, Click::Right, Click::Left];
        let spoken = spoken_click_sequence(&clicks);
        assert_eq!(spoken, "1. Left Click, 2. Right Click, 3. Left Click");
    }

    struct PointerPasskeyHarness {
        method: PasskeyMethod,
        pal: Palette,
    }

    impl Render for PointerPasskeyHarness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            passkey_panel(&self.method, self.pal)
        }
    }

    #[gpui::test]
    fn pointer_passkey_panel_renders(cx: &mut TestAppContext) {
        let pal = cx.update(|cx| {
            gpui_component::init(cx);
            theme::register_builtin_themes(cx);
            theme::palette(cx)
        });
        let method = PasskeyMethod::Pointer {
            passkey: "123".into(),
            clicks: vec![Click::Left, Click::Right, Click::Left],
        };
        let (_view, cx) = cx.add_window_view(|_, _| PointerPasskeyHarness { method, pal });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let bounds = cx
            .debug_bounds("passkey-sequence")
            .expect("passkey sequence must be laid out and painted");
        assert!(bounds.size.width > gpui::px(0.));
        assert!(bounds.size.height > gpui::px(0.));
    }
}
