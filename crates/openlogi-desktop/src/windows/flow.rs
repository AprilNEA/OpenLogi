//! The Flow window: where this computer sits among its peers, which devices
//! follow the pointer to each one, and whether Flow can switch now.
//!
//! Every change goes through [`AppState`]'s `commit_flow_*` methods into
//! `[flow]`; each peer's link state comes from the agent's snapshot. Peers are
//! paired outside this window for now: they come from `config.toml`.

mod arrangement;
mod inspector;
#[cfg(test)]
mod tests;

use gpui::{
    App, Context, FocusHandle, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, Size, Styled as _, Subscription, Window, div,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::Selectable as _;
use gpui_component::{
    Icon, IconName, Root, Sizable as _,
    button::Button,
    h_flex,
    radio::{Radio, RadioGroup},
    v_flex,
};
use openlogi_core::config::{FlowConfig, FlowEdge, FlowPeerName};
use openlogi_ipc::FlowLinkState;

use crate::app::menu::{CloseWindow, Minimize, Zoom};
use crate::state::{
    AppState, FlowDeviceRow, FlowHealth, FlowHost, FlowIssue, FlowReadiness, StateEvent,
};
use crate::ui::components::Toggle;
use crate::ui::theme::{self, Palette, Typography as _};
use crate::windows::{self, AuxWindow};

/// Open the Flow window, or focus it when it is already open.
pub fn open(cx: &mut App) {
    windows::open_or_focus(
        |reg| &mut reg.flow,
        window_title(),
        Size::new(px(980.), px(640.)),
        FlowView::new,
        cx,
    );
}

/// The window's native title — one definition for open and the live-language
/// retitle ([`windows::retitle_open`]), so the two cannot drift.
pub(crate) fn window_title() -> SharedString {
    tr!("flow.title")
}

/// What one render of the window shows, read from [`AppState`] once.
struct FlowModel {
    config: FlowConfig,
    readiness: FlowReadiness,
    devices: Vec<FlowDeviceRow>,
    peers: Vec<PeerModel>,
}

struct PeerModel {
    name: FlowPeerName,
    /// Every edge that leads to the peer; the window makes one, but a
    /// hand-edited `[flow.layout]` can name several.
    edges: Vec<FlowEdge>,
    link: Option<FlowLinkState>,
    /// The agent arms this peer's edge: connected, with a device to hand over.
    armed: bool,
}

impl FlowModel {
    fn read(state: &AppState) -> Self {
        let config = state.flow_config().clone();
        let readiness = state.flow_readiness();
        let peers = config
            .peers()
            .iter()
            .map(|peer| PeerModel {
                name: peer.name.clone(),
                edges: config
                    .layout()
                    .iter()
                    .filter(|layout| layout.peer == peer.name)
                    .map(|layout| layout.edge)
                    .collect(),
                link: state.flow_link(peer.name.as_str()),
                armed: config.enabled
                    && readiness.routes.iter().any(|route| route.peer == peer.name),
            })
            .collect();
        Self {
            devices: state.flow_devices(),
            config,
            readiness,
            peers,
        }
    }

    fn peer(&self, name: &FlowPeerName) -> Option<&PeerModel> {
        self.peers.iter().find(|peer| &peer.name == name)
    }
}

/// Standalone Flow window root view.
pub struct FlowView {
    focus_handle: FocusHandle,
    /// The computer the inspector shows.
    selected: FlowHost,
    appearance_obs: Option<Subscription>,
    #[expect(dead_code, reason = "held to repaint on Flow and agent changes")]
    state_obs: Subscription,
}

impl FlowView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let state_obs = cx.subscribe(&AppState::global(cx), |_, _, event: &StateEvent, cx| {
            if matches!(
                event,
                StateEvent::FlowChanged
                    | StateEvent::AgentChanged
                    | StateEvent::InventoryChanged
                    | StateEvent::SettingsChanged
            ) {
                cx.notify();
            }
        });
        Self {
            focus_handle,
            selected: FlowHost::This,
            appearance_obs: None,
            state_obs,
        }
    }

    pub(super) fn select(&mut self, host: FlowHost, cx: &mut Context<Self>) {
        if self.selected != host {
            self.selected = host;
            cx.notify();
        }
    }
}

impl AuxWindow for FlowView {
    fn set_appearance_obs(&mut self, sub: Subscription) {
        self.appearance_obs = Some(sub);
    }
}

impl Render for FlowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        theme::apply_ui_scale(window, cx);
        let pal = theme::palette(cx);
        let model = AppState::try_read(cx).map(FlowModel::read);
        // A forgotten peer cannot stay selected.
        if let (FlowHost::Peer(name), Some(model)) = (&self.selected, &model)
            && model.peer(name).is_none()
        {
            self.selected = FlowHost::This;
        }

        v_flex()
            .size_full()
            .bg(pal.page)
            .text_color(pal.text_primary)
            .track_focus(&self.focus_handle)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            .when(windows::needs_client_titlebar(window), |this| {
                this.child(windows::aux_title_bar(window_title(), cx))
            })
            .when_some(model, |this, model| {
                this.child(toolbar(&model, pal)).child(
                    h_flex()
                        .flex_1()
                        .min_h_0()
                        .items_stretch()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(arrangement::canvas(&model, &self.selected, pal, cx))
                                .child(footer(&model, pal)),
                        )
                        .child(inspector::panel(&model, &self.selected, pal, window, cx)),
                )
            })
            // Root owns the dialog state, but each window mounts its layer:
            // without it the remove confirmation never paints.
            .children(Root::render_dialog_layer(window, cx))
    }
}

/// The switch that turns Flow on, and its status in one line.
fn toolbar(model: &FlowModel, pal: Palette) -> impl IntoElement {
    h_flex()
        .flex_none()
        .h(px(52.))
        .px_4()
        .gap_4()
        .items_center()
        .border_b_1()
        .border_color(pal.border)
        .bg(pal.panel)
        .child(
            div().debug_selector(|| "flow-enabled".into()).child(
                Toggle::new("flow-enabled")
                    .selected(model.config.enabled)
                    .label(tr!("flow.enable"))
                    .on_change(|enabled, _, cx| {
                        let enabled = *enabled;
                        AppState::apply(cx, |state| state.commit_flow_enabled(enabled));
                    }),
            ),
        )
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .text_caption()
                .text_color(pal.text_muted)
                .debug_selector(|| "flow-status".into())
                .when_some(health_color(model.readiness.health), |row, color| {
                    row.child(dot(color, false))
                })
                .child(status_text(&model.readiness)),
        )
}

/// The readiness summary and the trigger choice, under the arrangement.
fn footer(model: &FlowModel, pal: Palette) -> impl IntoElement {
    let require_modifier = model.config.require_modifier;
    v_flex()
        .flex_none()
        .px_4()
        .py_3()
        .gap_3()
        .border_t_1()
        .border_color(pal.border)
        // With no peer the arrangement's empty state already says so.
        .when(model.readiness.health != FlowHealth::NoPeers, |footer| {
            footer.child(summary(&model.readiness, pal))
        })
        .child(
            h_flex()
                .gap_4()
                .items_center()
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(tr!("flow.trigger")),
                )
                .child(
                    RadioGroup::horizontal("flow-trigger")
                        .child(
                            Radio::new("flow-trigger-edge")
                                .label(tr!("flow.trigger_edge"))
                                .debug_selector(|| "flow-trigger-edge".into()),
                        )
                        .child(
                            Radio::new("flow-trigger-modifier")
                                .label(tr!("flow.trigger_modifier"))
                                .debug_selector(|| "flow-trigger-modifier".into()),
                        )
                        .selected_index(Some(usize::from(require_modifier)))
                        .on_change(|index, _, cx| {
                            let required = *index == 1;
                            AppState::apply(cx, |state| {
                                state.commit_flow_require_modifier(required)
                            });
                        }),
                ),
        )
}

/// What Flow does now, and what keeps each peer that does not work from
/// working.
fn summary(readiness: &FlowReadiness, pal: Palette) -> impl IntoElement {
    let (icon, color) = match readiness.health {
        FlowHealth::Ready => (IconName::CircleCheck, rgb(theme::STATUS_CONNECTED).into()),
        FlowHealth::Blocked => (
            IconName::TriangleAlert,
            rgb(theme::STATUS_CONNECTING).into(),
        ),
        FlowHealth::Off | FlowHealth::NoPeers => (IconName::Info, pal.text_muted),
    };
    let headline: Vec<SharedString> = match readiness.health {
        FlowHealth::Off => vec![tr!("flow.summary_off")],
        FlowHealth::NoPeers => Vec::new(),
        FlowHealth::Blocked => vec![tr!("flow.summary_blocked")],
        FlowHealth::Ready => readiness
            .routes
            .iter()
            .map(|route| {
                let name = route.peer.to_string();
                match route.edge {
                    FlowEdge::Left => tr!("flow.route_left", name => name),
                    FlowEdge::Right => tr!("flow.route_right", name => name),
                    FlowEdge::Top => tr!("flow.route_top", name => name),
                    FlowEdge::Bottom => tr!("flow.route_bottom", name => name),
                }
            })
            .collect(),
    };
    h_flex()
        .debug_selector(|| "flow-summary".into())
        .gap_3()
        .items_start()
        .child(
            div()
                .pt(px(2.))
                .child(Icon::new(icon).size_4().text_color(color)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .children(
                    headline
                        .into_iter()
                        .map(|line| div().text_body().child(line)),
                )
                .children(readiness.issues.iter().map(|issue| {
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(issue_text(issue))
                })),
        )
}

fn status_text(readiness: &FlowReadiness) -> SharedString {
    match readiness.health {
        FlowHealth::Off => tr!("flow.status_off"),
        FlowHealth::NoPeers => tr!("flow.status_no_peers"),
        FlowHealth::Blocked => tr!("flow.status_blocked"),
        FlowHealth::Ready => tr!(
            "flow.status_ready",
            count => readiness
                .routes
                .iter()
                .map(|route| &route.peer)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                .to_string(),
        ),
    }
}

fn issue_text(issue: &FlowIssue) -> SharedString {
    match issue {
        FlowIssue::Unplaced(peer) => tr!("flow.issue_unplaced", name => peer.to_string()),
        FlowIssue::NoDevice(peer) => tr!("flow.issue_no_device", name => peer.to_string()),
        FlowIssue::Unreachable { peer, link } => match link {
            Some(FlowLinkState::Degraded) => {
                tr!("flow.issue_unstable", name => peer.to_string())
            }
            Some(FlowLinkState::Lost) => tr!("flow.issue_offline", name => peer.to_string()),
            _ => tr!("flow.issue_unknown", name => peer.to_string()),
        },
        FlowIssue::CannotSwitch { device, peer } => tr!(
            "flow.issue_cannot_switch",
            device => device.clone(),
            name => peer.to_string(),
        ),
        FlowIssue::DevicesAway(peer) => tr!("flow.issue_devices_away", name => peer.to_string()),
        FlowIssue::NotFollowing { device, peer } => tr!(
            "flow.issue_not_following",
            device => device.clone(),
            name => peer.to_string(),
        ),
    }
}

/// The colour of a status dot for `health`; none when Flow is off or has no
/// peer, which needs no attention.
pub(crate) fn health_color(health: FlowHealth) -> Option<Hsla> {
    match health {
        FlowHealth::Ready => Some(rgb(theme::STATUS_CONNECTED).into()),
        FlowHealth::Blocked => Some(rgb(theme::STATUS_CONNECTING).into()),
        FlowHealth::Off | FlowHealth::NoPeers => None,
    }
}

/// Flow's status in a few words, for the window's toolbar and the main
/// window's Flow button.
pub(crate) fn status_label(readiness: &FlowReadiness) -> SharedString {
    status_text(readiness)
}

fn edge_label(edge: FlowEdge) -> SharedString {
    match edge {
        FlowEdge::Left => tr!("flow.edge_left"),
        FlowEdge::Right => tr!("flow.edge_right"),
        FlowEdge::Top => tr!("flow.edge_top"),
        FlowEdge::Bottom => tr!("flow.edge_bottom"),
    }
}

fn link_label(link: Option<FlowLinkState>) -> SharedString {
    match link {
        Some(FlowLinkState::Connected) => tr!("flow.link_connected"),
        Some(FlowLinkState::Degraded) => tr!("flow.link_unstable"),
        Some(FlowLinkState::Lost) => tr!("flow.link_offline"),
        None => tr!("flow.link_unknown"),
    }
}

/// Connected is a solid green dot, unstable a solid amber one, offline or
/// unknown a hollow grey ring, so the state never depends on hue alone.
fn link_dot(link: Option<FlowLinkState>) -> impl IntoElement {
    match link {
        Some(FlowLinkState::Connected) => dot(rgb(theme::STATUS_CONNECTED).into(), false),
        Some(FlowLinkState::Degraded) => dot(rgb(theme::STATUS_CONNECTING).into(), false),
        _ => dot(rgb(theme::STATUS_OFFLINE).into(), true),
    }
}

fn dot(color: Hsla, hollow: bool) -> gpui::Div {
    div()
        .flex_none()
        .size(px(8.))
        .rounded_full()
        .when(hollow, |dot| dot.border_1().border_color(color))
        .when(!hollow, |dot| dot.bg(color))
}

/// The Easy-Switch channel choice for one device and one computer: "off" and
/// one button per channel, each taken channel disabled.
fn channel_segment(
    device: &FlowDeviceRow,
    host: &FlowHost,
    cx: &App,
) -> gpui_component::button::ButtonGroup {
    use crate::ui::theme::SelectableStyle as _;
    use gpui_component::Disableable as _;
    use openlogi_core::config::FlowHostSlot;

    let current = device.slot(host);
    let base = match host {
        FlowHost::This => "flow-self",
        FlowHost::Peer(_) => "flow-peer",
    };
    let id = |suffix: &str| SharedString::from(format!("{base}-{}-{suffix}", device.key.as_str()));
    let off_label = match host {
        FlowHost::This => tr!("flow.channel_off_self"),
        FlowHost::Peer(_) => tr!("flow.channel_off"),
    };
    let off = {
        let key = device.key.clone();
        let host = host.clone();
        let selector = id("off");
        Button::new(selector.clone())
            .label(off_label)
            .selected(current.is_none())
            .selected_segment(current.is_none(), cx)
            .debug_selector(move || selector.to_string())
            .on_click(move |_, _, cx| {
                AppState::apply(cx, |state| state.commit_flow_channel(&key, &host, None));
            })
    };
    let slots = (0..FlowHostSlot::MAX_HOSTS).filter_map(|index| {
        let slot = FlowHostSlot::new(index).ok()?;
        let chosen = current == Some(slot);
        let key = device.key.clone();
        let host = host.clone();
        let selector = id(&(index + 1).to_string());
        Some(
            Button::new(selector.clone())
                .label(SharedString::from((index + 1).to_string()))
                .selected(chosen)
                .selected_segment(chosen, cx)
                .disabled(!chosen && (!device.switchable || device.slot_taken(&host, slot)))
                .debug_selector(move || selector.to_string())
                .on_click(move |_, _, cx| {
                    AppState::apply(cx, |state| {
                        state.commit_flow_channel(&key, &host, Some(slot))
                    });
                }),
        )
    });
    gpui_component::button::ButtonGroup::new(id("channels"))
        .outline()
        .xsmall()
        .child(off)
        .children(slots)
}
