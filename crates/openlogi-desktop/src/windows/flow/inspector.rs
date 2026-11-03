//! The inspector beside the arrangement: the selected computer and the
//! Easy-Switch channel each device uses to reach it.

use gpui::{
    App, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_kit::component::{
    Icon, IconName, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonGroup, ButtonVariant, ButtonVariants as _},
    dialog::DialogButtonProps,
    h_flex,
    scroll::ScrollableElement as _,
    v_flex,
};
use openlogi_core::config::FlowPeerName;
use openlogi_core::device::DeviceKind;

use super::{FlowModel, FlowView, PeerModel, channel_segment, edge_label, link_dot, link_label};
use crate::state::{AppState, FlowDeviceRow, FlowHost};
use crate::ui::theme::{Palette, SelectableStyle as _, Typography as _};

const EDGES: [openlogi_core::config::FlowEdge; 4] = [
    openlogi_core::config::FlowEdge::Left,
    openlogi_core::config::FlowEdge::Right,
    openlogi_core::config::FlowEdge::Top,
    openlogi_core::config::FlowEdge::Bottom,
];

pub(super) fn panel(
    model: &FlowModel,
    selected: &FlowHost,
    pal: Palette,
    _window: &mut Window,
    cx: &mut Context<FlowView>,
) -> impl IntoElement {
    let body = match selected {
        FlowHost::Peer(name) => model
            .peer(name)
            .map(|peer| peer_section(model, peer, pal, cx)),
        FlowHost::This => None,
    }
    .unwrap_or_else(|| this_section(model, pal, cx));
    v_flex()
        .debug_selector(|| "flow-inspector".into())
        .flex_none()
        .w(px(320.))
        .h_full()
        .overflow_y_scrollbar()
        .id("flow-inspector")
        .border_l_1()
        .border_color(pal.border)
        .bg(pal.panel)
        .p_4()
        .gap_5()
        .child(body)
}

fn this_section(model: &FlowModel, pal: Palette, cx: &App) -> gpui::AnyElement {
    v_flex()
        .gap_5()
        .child(div().text_heading().child(tr!("flow.this_computer")))
        .child(
            section(tr!("flow.self_channels"))
                .child(device_rows(model, &FlowHost::This, pal, cx))
                .child(caption(tr!("flow.self_channels_hint"), pal)),
        )
        .into_any_element()
}

fn peer_section(
    model: &FlowModel,
    peer: &PeerModel,
    pal: Palette,
    cx: &Context<FlowView>,
) -> gpui::AnyElement {
    let name = SharedString::from(peer.name.to_string());
    let host = FlowHost::Peer(peer.name.clone());
    v_flex()
        .gap_5()
        .child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_heading()
                        .truncate()
                        .debug_selector(|| "flow-inspector-name".into())
                        .child(name.clone()),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(link_dot(peer.link))
                        .child(link_label(peer.link)),
                ),
        )
        .child(
            section(tr!("flow.position"))
                .child(edge_segment(peer, cx))
                .when(peer.edges.len() > 1, |section| {
                    section.child(caption(
                        tr!("flow.position_several", name => name.to_string()),
                        pal,
                    ))
                })
                .child(caption(
                    tr!("flow.position_hint", name => name.to_string()),
                    pal,
                )),
        )
        .child(
            section(tr!("flow.devices"))
                .child(device_rows(model, &host, pal, cx))
                .child(caption(tr!("flow.channels_hint"), pal)),
        )
        .child(remove_button(&peer.name))
        .into_any_element()
}

fn section(title: SharedString) -> gpui::Div {
    v_flex().gap_2().child(div().text_subheading().child(title))
}

fn caption(text: SharedString, pal: Palette) -> impl IntoElement {
    div().text_caption().text_color(pal.text_muted).child(text)
}

fn edge_segment(peer: &PeerModel, cx: &App) -> ButtonGroup {
    ButtonGroup::new("flow-edge")
        .outline()
        .small()
        .children(EDGES.map(|edge| {
            let chosen = peer.edges.contains(&edge);
            let name = peer.name.clone();
            let selector = format!("flow-edge-{edge:?}");
            Button::new(SharedString::from(selector.clone()))
                .label(edge_label(edge))
                .selected(chosen)
                .selected_segment(chosen, cx)
                .debug_selector(move || selector.clone())
                .on_click(move |_, _, cx| {
                    AppState::apply(cx, |state| state.commit_flow_edge(&name, edge));
                })
        }))
}

fn device_rows(model: &FlowModel, host: &FlowHost, pal: Palette, cx: &App) -> gpui::Div {
    if model.devices.is_empty() {
        return v_flex().child(caption(tr!("flow.no_devices"), pal));
    }
    v_flex().gap_3().children(
        model
            .devices
            .iter()
            .map(|device| device_row(device, host, pal, cx)),
    )
}

fn device_row(device: &FlowDeviceRow, host: &FlowHost, pal: Palette, cx: &App) -> impl IntoElement {
    let icon = if device.kind.is_some_and(DeviceKind::is_keyboard_family) {
        "action-icons/keyboard.svg"
    } else {
        "action-icons/mouse.svg"
    };
    // A peer row says which channel reaches this computer, the one the peer's
    // channel must differ from.
    let note = match (host, device.config.as_ref()) {
        _ if !device.switchable => Some(tr!("flow.device_cannot_switch")),
        (FlowHost::Peer(_), Some(config)) => Some(tr!(
            "flow.self_channel_note",
            channel => (config.self_slot().get() + 1).to_string(),
        )),
        _ => None,
    };
    h_flex()
        .gap_3()
        .items_center()
        .child(
            Icon::empty()
                .path(icon)
                .size_4()
                .flex_none()
                .text_color(pal.text_muted),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_body()
                        .truncate()
                        .child(SharedString::from(device.name.clone())),
                )
                .when_some(note, |col, note| col.child(caption(note, pal))),
        )
        .child(div().flex_none().child(channel_segment(device, host, cx)))
}

fn remove_button(peer: &FlowPeerName) -> impl IntoElement {
    let peer = peer.clone();
    h_flex().child(
        Button::new("flow-remove")
            .ghost()
            .small()
            .icon(IconName::Delete)
            .label(tr!("flow.remove"))
            .debug_selector(|| "flow-remove".into())
            .on_click(move |_, window, cx| open_remove_confirmation(window, cx, peer.clone())),
    )
}

/// Confirm before forgetting a peer: its key goes, so using it again means
/// pairing again.
fn open_remove_confirmation(window: &mut Window, cx: &mut App, peer: FlowPeerName) {
    let name = peer.to_string();
    window.open_alert_dialog(cx, move |alert, _, _| {
        alert
            .title(tr!("flow.remove_title", name => name.clone()))
            .description(tr!("flow.remove_body"))
            .button_props(
                DialogButtonProps::default()
                    .ok_text(tr!("flow.remove_confirm"))
                    .ok_variant(ButtonVariant::Danger)
                    .cancel_text(tr!("common.cancel"))
                    .show_cancel(true),
            )
            .on_ok({
                let peer = peer.clone();
                move |_, _, cx| {
                    AppState::apply(cx, |state| state.commit_flow_forget(&peer));
                    true
                }
            })
    });
}
