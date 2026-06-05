//! The "Add device" window — drives a wireless pairing session.
//!
//! Pairing runs on the long-lived [`crate::watchers::pairing`] thread. This
//! window is a thin state machine over two globals:
//!
//! - [`PairingControl`] — the channel the buttons push [`Control`] into
//!   (start / pick a device / cancel).
//! - [`PairingUi`] — the latest session state, updated from the pairing event
//!   stream in [`crate::main`]'s loop via [`apply_event`]. The view observes it
//!   and repaints on every change.
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
use openlogi_hid::{
    Click, DiscoveredDevice, PairingError, PairingEvent, PasskeyMethod, ReceiverSelector,
    WindowsPairingDevice, WindowsPairingError, WindowsPairingStatus,
};

use crate::theme::{self, Palette};
use crate::watchers::pairing::Control;
use crate::windows::{self, AuxWindow};

/// Sender side of the pairing watcher, published as a global so the window's
/// buttons can drive the session without threading a handle through the views.
pub struct PairingControl(pub tokio::sync::mpsc::UnboundedSender<Control>);

impl Global for PairingControl {}

/// The pairing flow's current UI state. Mirrors the [`PairingEvent`] stream.
#[derive(Clone, Default)]
pub enum PairingUi {
    /// No session in flight (initial, or after Done / dismissing a failure).
    #[default]
    Idle,
    /// Discovery (Bolt) or the pairing lock (Unifying) is open.
    Searching,
    /// Bolt: devices discovered so far, awaiting the user's pick.
    Found(Vec<DiscoveredDevice>),
    /// A device was picked; waiting for the receiver's next step.
    Pairing,
    /// Bolt: the device asks the user to enter a passkey.
    Passkey(PasskeyMethod),
    /// A device paired into `slot`.
    Paired { slot: u8 },
    /// Windows Bluetooth device enumeration is running.
    WindowsSearching,
    /// Windows Bluetooth candidates, awaiting the user's pick.
    WindowsFound(Vec<WindowsPairingDevice>),
    /// Windows is running the OS pairing ceremony.
    WindowsPairing { name: String },
    /// Windows reported a paired or already-paired result.
    WindowsPaired {
        name: String,
        status: WindowsPairingStatus,
    },
    /// The session ended without pairing; carries a human-readable detail.
    Failed {
        detail: SharedString,
        retryable: bool,
    },
}

impl Global for PairingUi {}

/// Open the Add Device window, starting a fresh search unless one is already
/// in flight (re-opening just focuses the existing window).
pub fn open(cx: &mut App) {
    let active = matches!(
        cx.try_global::<PairingUi>(),
        Some(
            PairingUi::Searching
                | PairingUi::Found(_)
                | PairingUi::Pairing
                | PairingUi::Passkey(_)
                | PairingUi::WindowsSearching
                | PairingUi::WindowsFound(_)
                | PairingUi::WindowsPairing { .. }
        )
    );
    if !active {
        start_search(cx);
    }
    windows::open_or_focus(
        |reg| &mut reg.add_device,
        tr!("Add Device"),
        Size::new(px(560.), px(480.)),
        AddDeviceView::new,
        cx,
    );
}

/// Fold a pairing event into [`PairingUi`]. Called from the GPUI event loop.
pub fn apply_event(cx: &mut App, event: PairingEvent) {
    let current = cx.try_global::<PairingUi>().cloned().unwrap_or_default();
    let next = match event {
        PairingEvent::Searching => PairingUi::Searching,
        PairingEvent::DeviceFound(device) => {
            let mut devices = match current {
                PairingUi::Found(devices) => devices,
                _ => Vec::new(),
            };
            if !devices.iter().any(|d| d.address == device.address) {
                devices.push(device);
            }
            PairingUi::Found(devices)
        }
        PairingEvent::Passkey(method) => PairingUi::Passkey(method),
        PairingEvent::Paired { slot } => PairingUi::Paired { slot },
        PairingEvent::WindowsSearching => PairingUi::WindowsSearching,
        PairingEvent::WindowsDeviceFound(device) => {
            let mut devices = match current {
                PairingUi::WindowsFound(devices) => devices,
                _ => Vec::new(),
            };
            if !devices.iter().any(|d| d.id == device.id) {
                devices.push(device);
            }
            PairingUi::WindowsFound(devices)
        }
        PairingEvent::WindowsPairing { name } => PairingUi::WindowsPairing { name },
        PairingEvent::WindowsPaired { name, status } => PairingUi::WindowsPaired { name, status },
        PairingEvent::Failed(error) => failure_state(&error),
    };
    cx.set_global(next);
}

fn failure_state(error: &PairingError) -> PairingUi {
    let detail = match error {
        PairingError::Hid(detail) => {
            tr!("HID transport error: %{error}", error => detail.clone())
        }
        PairingError::Timeout => {
            tr!("No device was found. Put the device in pairing mode and try again.")
        }
        PairingError::ReceiverNotFound => tr!("No supported Logitech receiver was found."),
        PairingError::Register(detail) => {
            tr!("Receiver register access failed: %{error}", error => detail.clone())
        }
        PairingError::Device(code) => tr!(
            "Receiver reported pairing error %{code}.",
            code => format!("0x{code:02x}")
        ),
        PairingError::Windows(error) => windows_pairing_error_detail(error),
        PairingError::WindowsStatus(status) => tr!(
            "Windows returned %{status}.",
            status => windows_pairing_status_text(*status)
        ),
        PairingError::Cancelled => tr!("Pairing was cancelled."),
    };
    PairingUi::Failed {
        detail,
        retryable: !matches!(error, PairingError::ReceiverNotFound),
    }
}

fn windows_pairing_error_detail(error: &WindowsPairingError) -> SharedString {
    match error {
        WindowsPairingError::Unsupported => {
            tr!("Windows Bluetooth pairing is only available on Windows.")
        }
        WindowsPairingError::NoCandidates => {
            tr!("No Windows Bluetooth pairing candidates were found.")
        }
        WindowsPairingError::Timeout => tr!("Windows pairing timed out."),
        WindowsPairingError::NotFound(name) => {
            tr!("Windows device not found: %{name}", name => name.clone())
        }
        WindowsPairingError::NotPairable(name) => {
            tr!("Windows device cannot pair: %{name}", name => name.clone())
        }
        WindowsPairingError::Api(error) => {
            tr!("Windows API error: %{error}", error => error.clone())
        }
    }
}

fn windows_pairing_status_text(status: WindowsPairingStatus) -> String {
    match status {
        WindowsPairingStatus::Paired => rust_i18n::t!("paired").into_owned(),
        WindowsPairingStatus::NotReadyToPair => rust_i18n::t!("not ready to pair").into_owned(),
        WindowsPairingStatus::NotPaired => rust_i18n::t!("not paired").into_owned(),
        WindowsPairingStatus::AlreadyPaired => rust_i18n::t!("already paired").into_owned(),
        WindowsPairingStatus::ConnectionRejected => {
            rust_i18n::t!("connection rejected").into_owned()
        }
        WindowsPairingStatus::TooManyConnections => {
            rust_i18n::t!("too many connections").into_owned()
        }
        WindowsPairingStatus::HardwareFailure => rust_i18n::t!("hardware failure").into_owned(),
        WindowsPairingStatus::AuthenticationTimeout => {
            rust_i18n::t!("authentication timed out").into_owned()
        }
        WindowsPairingStatus::AuthenticationNotAllowed => {
            rust_i18n::t!("authentication not allowed").into_owned()
        }
        WindowsPairingStatus::AuthenticationFailure => {
            rust_i18n::t!("authentication failed").into_owned()
        }
        WindowsPairingStatus::NoSupportedProfiles => {
            rust_i18n::t!("no supported profiles").into_owned()
        }
        WindowsPairingStatus::ProtectionLevelCouldNotBeMet => {
            rust_i18n::t!("protection level could not be met").into_owned()
        }
        WindowsPairingStatus::AccessDenied => rust_i18n::t!("access denied").into_owned(),
        WindowsPairingStatus::InvalidCeremonyData => {
            rust_i18n::t!("invalid ceremony data").into_owned()
        }
        WindowsPairingStatus::PairingCanceled => rust_i18n::t!("pairing canceled").into_owned(),
        WindowsPairingStatus::OperationAlreadyInProgress => {
            rust_i18n::t!("operation already in progress").into_owned()
        }
        WindowsPairingStatus::RequiredHandlerNotRegistered => {
            rust_i18n::t!("required handler not registered").into_owned()
        }
        WindowsPairingStatus::RejectedByHandler => {
            rust_i18n::t!("rejected by handler").into_owned()
        }
        WindowsPairingStatus::RemoteDeviceHasAssociation => {
            rust_i18n::t!("remote device already has an association").into_owned()
        }
        WindowsPairingStatus::Failed => rust_i18n::t!("failed").into_owned(),
        WindowsPairingStatus::Other(code) => {
            rust_i18n::t!("unknown status %{code}", code => code.to_string()).into_owned()
        }
    }
}

fn send(cx: &App, control: Control) {
    if let Some(ctrl) = cx.try_global::<PairingControl>() {
        let _ = ctrl.0.send(control);
    }
}

fn start_search(cx: &mut App) {
    cx.set_global(PairingUi::Searching);
    send(cx, Control::Start(ReceiverSelector::First));
}

fn start_windows_search(cx: &mut App) {
    cx.set_global(PairingUi::WindowsSearching);
    send(cx, Control::StartWindows);
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
            .p_7()
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
#[allow(clippy::too_many_lines)]
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
            if cfg!(target_os = "windows") {
                col = col.child(
                    action_button("ad-windows-search", tr!("Windows Bluetooth"), pal, false)
                        .on_click(|_, _, cx| start_windows_search(cx)),
                );
            }
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
        PairingUi::WindowsSearching => {
            col = col
                .child(status_line(tr!("Searching Windows Bluetooth..."), pal))
                .child(hint(
                    tr!("Make sure the device is on and in pairing mode."),
                    pal,
                ))
                .child(cancel_button(pal));
        }
        PairingUi::WindowsFound(devices) => {
            col = col.child(status_line(tr!("Select a Windows Bluetooth device:"), pal));
            if devices.is_empty() {
                col = col.child(hint(tr!("No devices found yet..."), pal));
            } else {
                for (idx, device) in devices.iter().enumerate() {
                    col = col.child(windows_device_row(idx, device, pal));
                }
            }
            col = col.child(cancel_button(pal));
        }
        PairingUi::WindowsPairing { name } => {
            col = col
                .child(status_line(
                    tr!("Windows is pairing %{name}...", name => name.clone()),
                    pal,
                ))
                .child(hint(tr!("Follow the Windows pairing prompt."), pal))
                .child(cancel_button(pal));
        }
        PairingUi::WindowsPaired { name, status } => {
            col = col
                .child(
                    div()
                        .text_color(rgb(theme::STATUS_CONNECTED))
                        .font_weight(FontWeight::MEDIUM)
                        .child(tr!("Device paired")),
                )
                .child(hint(
                    tr!(
                        "%{name} paired through Windows Bluetooth (%{status}).",
                        name => name.clone(),
                        status => windows_pairing_status_text(*status)
                    ),
                    pal,
                ))
                .child(
                    action_button("ad-done", tr!("Done"), pal, false)
                        .on_click(|_, _, cx| cx.set_global(PairingUi::Idle)),
                );
        }
        PairingUi::Failed { detail, retryable } => {
            let title = if *retryable {
                tr!("Pairing failed")
            } else {
                tr!("Pairing unavailable")
            };
            col = col
                .child(
                    div()
                        .text_color(rgb(theme::STATUS_CONNECTING))
                        .font_weight(FontWeight::MEDIUM)
                        .child(title),
                )
                .child(hint(detail.clone(), pal));
            if *retryable {
                col = col.child(
                    action_button("ad-retry", tr!("Try again"), pal, true)
                        .on_click(|_, _, cx| start_search(cx)),
                );
                col = col.child(
                    action_button("ad-windows-retry", tr!("Windows Bluetooth"), pal, false)
                        .on_click(|_, _, cx| start_windows_search(cx)),
                );
            } else {
                col = col.child(
                    action_button("ad-done", tr!("Done"), pal, false)
                        .on_click(|_, _, cx| cx.set_global(PairingUi::Idle)),
                );
            }
        }
    }
    col
}

/// A discovered-device row; clicking it pairs with that device.
fn device_row(idx: usize, device: &DiscoveredDevice, pal: Palette) -> impl IntoElement {
    let picked = device.clone();
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
            send(cx, Control::Pair(picked.clone()));
            cx.set_global(PairingUi::Pairing);
        })
}

/// A Windows Bluetooth candidate row; clicking it asks Windows to pair it.
fn windows_device_row(idx: usize, device: &WindowsPairingDevice, pal: Palette) -> impl IntoElement {
    let picked = device.clone();
    let detail = if device.likely_logitech {
        tr!("Likely Logitech device")
    } else {
        tr!("Bluetooth device")
    };
    div()
        .id(("windows-device", idx))
        .w_full()
        .px_4()
        .py_3()
        .rounded_md()
        .border_1()
        .border_color(pal.border)
        .cursor_pointer()
        .hover(|s| s.bg(pal.surface_hover))
        .child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .child(SharedString::from(device.name.clone())),
                )
                .child(hint(detail, pal)),
        )
        .on_click(move |_, _, cx| {
            send(cx, Control::PairWindows(picked.clone()));
            cx.set_global(PairingUi::WindowsPairing {
                name: picked.name.clone(),
            });
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
        .line_height(gpui::relative(1.3))
        .font_weight(FontWeight::MEDIUM)
        .child(text.into())
}

fn hint(text: impl Into<SharedString>, pal: Palette) -> impl IntoElement {
    div()
        .text_xs()
        .line_height(gpui::relative(1.35))
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
        .min_w(px(190.))
        .rounded_md()
        .text_center()
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
        send(cx, Control::Cancel);
        cx.set_global(PairingUi::Idle);
    })
}
