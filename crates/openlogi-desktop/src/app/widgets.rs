//! Small leaf UI pieces shared between the Home and device-detail screens:
//! panel chrome, status pills, and the header buttons that appear on both
//! screens.

use gpui::{
    App, Context, InteractiveElement as _, IntoElement, ParentElement, RenderOnce, SharedString,
    Styled, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_kit::component::{
    Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
};
use openlogi_core::device::DeviceKind;
use openlogi_core::hid::{DeviceRoute, ReceiverBrand};

use super::AppView;
use crate::state::{AppState, DeviceRecord};
use crate::ui::components::control_button;
use crate::ui::theme::{self, Palette, Typography as _};

/// "← Devices" affordance on the detail screen; returns to the gallery without
/// changing the active-device selection.
pub(super) fn back_button(cx: &mut Context<AppView>) -> impl IntoElement {
    let view = cx.entity();
    Button::new("detail-back")
        .ghost()
        .small()
        .icon(IconName::ChevronLeft)
        .label(tr!("device.devices"))
        .on_click(move |_, _, cx| view.update(cx, AppView::go_home))
}

/// Settings button in the Home header: opens the Settings window. The visible
/// label keeps the action discoverable without requiring hover.
pub(super) fn settings_button() -> impl IntoElement {
    Button::new("home-settings")
        .outline()
        .icon(IconName::Settings)
        .label(tr!("app.settings"))
        .tooltip(tr!("app.settings"))
        .debug_selector(|| "home-settings".into())
        .on_click(|_, _, cx| crate::windows::settings::open(cx))
}

/// Opens the Flow window. Its dot repeats Flow's status — green when the
/// pointer can cross to a peer, amber when something blocks it — and the
/// tooltip and accessible label say it in words.
pub(super) fn flow_button() -> impl IntoElement {
    FlowButton
}

#[derive(IntoElement)]
struct FlowButton;

impl RenderOnce for FlowButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let readiness = AppState::try_read(cx).map(AppState::flow_readiness);
        let status = readiness
            .as_ref()
            .map_or_else(Default::default, crate::windows::flow::status_label);
        let color = readiness
            .as_ref()
            .and_then(|readiness| crate::windows::flow::health_color(readiness.health));
        let label = tr!("flow.button_label", status => status.to_string());
        Button::new("home-flow")
            .outline()
            .icon(Icon::empty().path("action-icons/arrow-left-right.svg"))
            .label(tr!("flow.title"))
            .when_some(color, |button, color| {
                button.child(div().size(px(8.)).rounded_full().bg(color))
            })
            .tooltip(label)
            .debug_selector(|| "home-flow".into())
            .on_click(|_, _, cx| crate::windows::flow::open(cx))
    }
}

/// Whether the agent answers right now. Pairing runs in the agent, and only a
/// live link confirms what the window last heard about a device, so every Add
/// Device entry and connectivity readout asks here.
pub(super) fn agent_live(state: &AppState) -> bool {
    state.agent_status().is_some()
}

/// Primary action that opens the pairing window, in the Home header and as
/// the empty state's call to action; `id` tells the two apart.
pub(super) fn add_device_button(id: &'static str) -> impl IntoElement {
    AddDeviceButton { id }
}

/// [`add_device_button`]: pairing runs in the agent, so without a live
/// connection the button is disabled and its tooltip says why.
#[derive(IntoElement)]
struct AddDeviceButton {
    id: &'static str,
}

impl RenderOnce for AddDeviceButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let live = AppState::try_read(cx).is_some_and(agent_live);
        let id = self.id;
        Button::new(id)
            .primary()
            .icon(IconName::Plus)
            .label(tr!("pairing.add_device"))
            .disabled(!live)
            .tooltip(if live {
                tr!("pairing.add_device")
            } else {
                tr!("pairing.pairing_agent_not_ready")
            })
            .debug_selector(move || id.into())
            .on_click(|_, _, cx| crate::windows::add_device::open(cx))
    }
}

pub(super) fn main_window_title(show_device: bool, cx: &Context<AppView>) -> SharedString {
    if !show_device {
        return SharedString::from("OpenLogi");
    }
    AppState::try_global(cx)
        .map(|state| state.read(cx))
        .and_then(AppState::current_record)
        .map_or_else(
            || SharedString::from("OpenLogi"),
            |record| SharedString::from(format!("OpenLogi - {}", record.display_name)),
        )
}

/// What the window may say about a device's connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Connectivity {
    Connected,
    /// Connected when the agent link dropped. The last state stays on
    /// screen, but nothing confirms it until the agent answers again.
    Reconnecting,
    Offline,
}

impl Connectivity {
    /// A device the agent last reported `online`, while the link is
    /// `agent_live` or not.
    pub(super) fn new(online: bool, agent_live: bool) -> Self {
        match (online, agent_live) {
            (false, _) => Self::Offline,
            (true, false) => Self::Reconnecting,
            (true, true) => Self::Connected,
        }
    }

    pub(super) fn label(self) -> SharedString {
        match self {
            Self::Connected => tr!("device.connected"),
            Self::Reconnecting => tr!("agent.reconnecting_to_the_background_service"),
            Self::Offline => tr!("device.offline"),
        }
    }
}

/// The detail screen's connectivity pill. It reads the agent link itself, so
/// a lost agent turns "Connected" into the reconnecting state wherever the
/// pill shows.
pub(super) fn status_badge(online: bool, pal: Palette) -> impl IntoElement {
    StatusBadge { online, pal }
}

#[derive(IntoElement)]
struct StatusBadge {
    online: bool,
    pal: Palette,
}

impl RenderOnce for StatusBadge {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { online, pal } = self;
        let connectivity =
            Connectivity::new(online, AppState::try_read(cx).is_some_and(agent_live));
        h_flex()
            .gap_1()
            .items_center()
            .rounded_full()
            .border_1()
            .border_color(pal.border)
            .px_2()
            .py_1()
            .text_caption()
            .text_color(pal.text_muted)
            .child(connectivity_dot(connectivity, pal))
            .child(connectivity.label())
    }
}

/// Connectivity indicator: connected is solid and offline is hollow, so those
/// never depend on hue alone. Reconnecting takes the connecting amber and is
/// always named in words too, beside the dot or in its card's hover tip.
pub(super) fn connectivity_dot(connectivity: Connectivity, pal: Palette) -> impl IntoElement {
    let dot = div().size_1p5().rounded_full().border_1();
    match connectivity {
        Connectivity::Connected => dot.border_color(pal.text_muted).bg(pal.text_primary),
        Connectivity::Reconnecting => dot
            .border_color(rgb(theme::STATUS_CONNECTING))
            .bg(rgb(theme::STATUS_CONNECTING))
            .debug_selector(|| "device-reconnecting".into()),
        Connectivity::Offline => dot.border_color(pal.text_muted),
    }
}

pub(super) fn sidebar_action(
    id: &'static str,
    icon: IconName,
    label: SharedString,
    handler: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    control_button(id)
        .icon(icon)
        .label(label)
        .on_click(handler)
        .flex_1()
}

pub(super) fn route_label(
    route: Option<&DeviceRoute>,
    receiver_brand: Option<ReceiverBrand>,
) -> String {
    match route {
        Some(DeviceRoute::Bolt { .. }) => tr!("device.bolt_receiver").to_string(),
        Some(DeviceRoute::Unifying { .. }) => match receiver_brand {
            Some(ReceiverBrand::Nano) => tr!("device.nano_receiver").to_string(),
            Some(ReceiverBrand::Lightspeed) => tr!("device.lightspeed_receiver").to_string(),
            Some(ReceiverBrand::Bolt | ReceiverBrand::Unifying) | None => {
                tr!("device.unifying_receiver").to_string()
            }
        },
        Some(DeviceRoute::Direct { .. } | DeviceRoute::RawHid { .. }) => {
            tr!("device.direct_connection").to_string()
        }
        None => tr!("common.unavailable").to_string(),
    }
}

/// The receiver pairing slot a device sits in — "Slot 4" — or `None` for a
/// device with no receiver between it and the host.
///
/// Called a slot, not a channel: a receiver slot is unrelated to the
/// device's own Easy-Switch channel, and the two are easy to confuse.
pub(super) fn receiver_slot_label(record: &DeviceRecord) -> Option<SharedString> {
    matches!(
        record.route,
        Some(DeviceRoute::Bolt { .. } | DeviceRoute::Unifying { .. })
    )
    .then(|| tr!("device.receiver_slot_number", number => record.slot.to_string()))
}

/// How a device is reached, in words: the transport, plus the receiver slot
/// when there is one ("Bolt receiver · Slot 4").
pub(super) fn connection_summary(record: &DeviceRecord) -> String {
    let route = route_label(record.route.as_ref(), record.receiver_brand);
    match receiver_slot_label(record) {
        Some(slot) => format!("{route} · {slot}"),
        None => route,
    }
}

pub(super) fn kind_label(kind: DeviceKind) -> String {
    tr!(kind.translation_key()).to_string()
}
