//! The arrangement: this computer in the centre and each peer beyond the edge
//! of this computer's screen that leads to it.
//!
//! A peer moves by dragging its tile onto another edge, or with the arrow
//! keys once it is selected. Peers with no edge wait in a tray underneath.

use gpui::{
    AppContext as _, Context, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div, prelude::FluentBuilder as _, px,
};
use gpui_base::Button as BaseButton;
use gpui_component::{Icon, h_flex, v_flex};
use openlogi_core::config::{FlowEdge, FlowPeerName};

use super::{FlowModel, FlowView, PeerModel, link_dot, link_label};
use crate::state::{AppState, FlowHost};
use crate::ui::theme::{self, Palette, Typography as _};

const TILE_W: f32 = 136.;
const SCREEN_W: f32 = 120.;
const SCREEN_H: f32 = 68.;
const TILE_H: f32 = SCREEN_H + 44.;
const GAP_X: f32 = 24.;
const GAP_Y: f32 = 12.;
const CANVAS_W: f32 = 3. * TILE_W + 2. * GAP_X;
const CANVAS_H: f32 = 3. * TILE_H + 2. * GAP_Y;
const EDGES: [FlowEdge; 4] = [
    FlowEdge::Left,
    FlowEdge::Right,
    FlowEdge::Top,
    FlowEdge::Bottom,
];

/// Where the tile beyond `edge` sits, or the centre tile for `None`.
fn origin(edge: Option<FlowEdge>) -> (f32, f32) {
    let centre = (TILE_W + GAP_X, TILE_H + GAP_Y);
    match edge {
        None => centre,
        Some(FlowEdge::Left) => (0., centre.1),
        Some(FlowEdge::Right) => (2. * (TILE_W + GAP_X), centre.1),
        Some(FlowEdge::Top) => (centre.0, 0.),
        Some(FlowEdge::Bottom) => (centre.0, 2. * (TILE_H + GAP_Y)),
    }
}

/// A peer tile being dragged to another edge.
#[derive(Clone)]
pub(super) struct DraggedPeer {
    name: FlowPeerName,
    pal: Palette,
}

impl Render for DraggedPeer {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        screen(self.pal, false, false, false)
            .opacity(0.85)
            .shadow_md()
            .child(
                div()
                    .px_2()
                    .text_caption()
                    .truncate()
                    .child(SharedString::from(self.name.to_string())),
            )
    }
}

pub(super) fn canvas(
    model: &FlowModel,
    selected: &FlowHost,
    pal: Palette,
    cx: &mut Context<FlowView>,
) -> impl IntoElement {
    let dragging = cx.has_active_drag();
    let placed: Vec<&PeerModel> = model
        .peers
        .iter()
        .filter(|peer| peer.edge.is_some())
        .collect();
    let unplaced: Vec<&PeerModel> = model
        .peers
        .iter()
        .filter(|peer| peer.edge.is_none())
        .collect();
    let mut grid = div()
        .relative()
        .flex_none()
        .w(px(CANVAS_W))
        .h(px(CANVAS_H))
        .children(
            placed
                .iter()
                .filter_map(|peer| Some(edge_bar(peer.edge?, peer.armed, pal))),
        );
    grid = grid.child(this_tile(*selected == FlowHost::This, pal, cx));
    for peer in &placed {
        grid = grid.child(peer_tile(peer, selected, pal, cx));
    }
    if dragging {
        for edge in EDGES {
            grid = grid.child(drop_slot(edge, pal, cx));
        }
    }
    if model.peers.is_empty() {
        grid = grid.child(empty_state(pal));
    }

    let chips: Vec<_> = unplaced
        .iter()
        .map(|peer| tray_chip(peer, selected, pal, cx))
        .collect();

    v_flex()
        .id("flow-arrangement")
        .debug_selector(|| "flow-arrangement".into())
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap_4()
        .p_4()
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
            let edge = match event.keystroke.key.as_str() {
                "left" => FlowEdge::Left,
                "right" => FlowEdge::Right,
                "up" => FlowEdge::Top,
                "down" => FlowEdge::Bottom,
                _ => return,
            };
            let FlowHost::Peer(peer) = this.selected.clone() else {
                return;
            };
            cx.stop_propagation();
            AppState::apply(cx, |state| state.commit_flow_edge(&peer, edge));
        }))
        .child(grid)
        .when(!unplaced.is_empty(), |this| {
            this.child(
                v_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        h_flex()
                            .flex_wrap()
                            .justify_center()
                            .gap_2()
                            .children(chips),
                    )
                    .child(
                        div()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(tr!("flow.unplaced_hint")),
                    ),
            )
        })
}

/// With no peer, the row under this computer says how one gets here.
fn empty_state(pal: Palette) -> impl IntoElement {
    let (_, y) = origin(Some(FlowEdge::Bottom));
    v_flex()
        .absolute()
        .left_0()
        .top(px(y))
        .w(px(CANVAS_W))
        .items_center()
        .gap_1()
        .text_center()
        .child(div().text_subheading().child(tr!("flow.empty_title")))
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("flow.empty_body")),
        )
}

/// A bar on this computer's screen marking an edge that leads to a peer, in
/// the accent colour once the agent arms it.
fn edge_bar(edge: FlowEdge, armed: bool, pal: Palette) -> gpui::Div {
    let (self_x, self_y) = origin(None);
    let screen_x = self_x + (TILE_W - SCREEN_W) / 2.;
    let (x, y, w, h) = match edge {
        FlowEdge::Left => (screen_x - 7., self_y + 10., 4., SCREEN_H - 20.),
        FlowEdge::Right => (screen_x + SCREEN_W + 3., self_y + 10., 4., SCREEN_H - 20.),
        FlowEdge::Top => (screen_x + 12., self_y - 7., SCREEN_W - 24., 4.),
        FlowEdge::Bottom => (screen_x + 12., self_y + SCREEN_H + 3., SCREEN_W - 24., 4.),
    };
    div()
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(w))
        .h(px(h))
        .rounded_full()
        .bg(if armed { theme::accent() } else { pal.border })
}

/// A monitor outline: the visual of one computer.
fn screen(pal: Palette, this: bool, selected: bool, quiet: bool) -> gpui::Div {
    div()
        .w(px(SCREEN_W))
        .h(px(SCREEN_H))
        .flex()
        .items_center()
        .justify_center()
        .rounded(pal.card_radius)
        .border_2()
        .border_color(if selected {
            theme::accent()
        } else if quiet {
            pal.border
        } else {
            pal.text_muted.opacity(0.6)
        })
        .bg(if this {
            theme::accent_tint()
        } else if quiet {
            pal.muted
        } else {
            pal.panel
        })
        .text_color(if quiet {
            pal.text_muted
        } else {
            pal.text_primary
        })
}

fn monitor_icon(pal: Palette) -> Icon {
    Icon::empty()
        .path("action-icons/monitor.svg")
        .size_5()
        .text_color(pal.text_muted)
}

fn tile_frame(id: SharedString, label: SharedString, edge: Option<FlowEdge>) -> BaseButton {
    let (x, y) = origin(edge);
    let selector = id.to_string();
    BaseButton::new(id)
        .accessibility_label(label)
        .debug_selector(move || selector.clone())
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(TILE_W))
        .h(px(TILE_H))
        .flex()
        .flex_col()
        .items_center()
        // Button centres its content; tiles keep their screens on one row
        // however many lines sit under them.
        .justify_start()
        .gap_1()
        .cursor_pointer()
}

fn this_tile(selected: bool, pal: Palette, cx: &Context<FlowView>) -> BaseButton {
    tile_frame("flow-tile-self".into(), tr!("flow.this_computer"), None)
        .selected(selected)
        .on_click(cx.listener(|this, _, _, cx| this.select(FlowHost::This, cx)))
        .focus_visible(|style| style.opacity(0.85))
        // The name sits inside the screen: below it is where the bar of a
        // bottom edge goes.
        .child(
            screen(pal, true, selected, false).child(
                div()
                    .px_2()
                    .truncate()
                    .text_caption()
                    .text_color(theme::accent())
                    .child(tr!("flow.this_computer")),
            ),
        )
}

fn peer_tile(
    peer: &PeerModel,
    selected: &FlowHost,
    pal: Palette,
    cx: &Context<FlowView>,
) -> BaseButton {
    let is_selected = matches!(selected, FlowHost::Peer(name) if *name == peer.name);
    let quiet = !matches!(peer.link, Some(openlogi_ipc::FlowLinkState::Connected));
    let name = SharedString::from(peer.name.to_string());
    let edge_word = peer.edge.map(super::edge_label).unwrap_or_default();
    let label = tr!(
        "flow.tile_label",
        name => name.to_string(),
        edge => edge_word.to_string(),
        status => link_label(peer.link).to_string(),
    );
    let host = FlowHost::Peer(peer.name.clone());
    let dragged = DraggedPeer {
        name: peer.name.clone(),
        pal,
    };
    tile_frame(tile_id(&peer.name), label, peer.edge)
        .selected(is_selected)
        .on_click(cx.listener(move |this, _, _, cx| this.select(host.clone(), cx)))
        .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
        .child(screen(pal, false, is_selected, quiet).child(monitor_icon(pal)))
        .child(
            div()
                .max_w(px(TILE_W))
                .truncate()
                .text_caption()
                .child(name),
        )
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .text_caption()
                .text_color(pal.text_muted)
                .child(link_dot(peer.link))
                .child(link_label(peer.link)),
        )
}

pub(super) fn tile_id(peer: &FlowPeerName) -> SharedString {
    SharedString::from(format!("flow-tile-{}", peer.as_str()))
}

/// A peer with no edge yet, waiting under the arrangement.
fn tray_chip(
    peer: &PeerModel,
    selected: &FlowHost,
    pal: Palette,
    cx: &Context<FlowView>,
) -> BaseButton {
    let is_selected = matches!(selected, FlowHost::Peer(name) if *name == peer.name);
    let name = SharedString::from(peer.name.to_string());
    let host = FlowHost::Peer(peer.name.clone());
    let dragged = DraggedPeer {
        name: peer.name.clone(),
        pal,
    };
    let selector = format!("flow-tray-{}", peer.name.as_str());
    BaseButton::new(SharedString::from(selector.clone()))
        .accessibility_label(tr!("flow.tray_label", name => name.to_string()))
        .debug_selector(move || selector.clone())
        .selected(is_selected)
        .on_click(cx.listener(move |this, _, _, cx| this.select(host.clone(), cx)))
        .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
        .h(px(theme::CONTROL_H))
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .max_w(px(280.))
        .rounded_full()
        .border_1()
        .border_color(if is_selected {
            theme::accent()
        } else {
            pal.border
        })
        .bg(pal.panel)
        .cursor_pointer()
        .hover(|style| style.bg(pal.control_hover))
        .text_caption()
        .child(
            Icon::empty()
                .path("action-icons/monitor.svg")
                .size_3p5()
                .text_color(pal.text_muted),
        )
        .child(div().min_w_0().truncate().child(name))
        .child(
            div()
                .flex_none()
                .text_color(pal.text_muted)
                .child(tr!("flow.unplaced")),
        )
}

/// Where a dragged peer can land: shown on every edge while a drag runs.
fn drop_slot(edge: FlowEdge, pal: Palette, cx: &Context<FlowView>) -> gpui::Stateful<gpui::Div> {
    let (x, y) = origin(Some(edge));
    let selector = format!("flow-slot-{edge:?}");
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .absolute()
        .left(px(x + (TILE_W - SCREEN_W) / 2.))
        .top(px(y))
        .w(px(SCREEN_W))
        .h(px(SCREEN_H))
        .rounded(pal.card_radius)
        .border_2()
        .border_color(theme::accent().opacity(0.35))
        .drag_over::<DraggedPeer>(|style, _, _, _| {
            style.bg(theme::accent_tint()).border_color(theme::accent())
        })
        .on_drop(cx.listener(move |_, dragged: &DraggedPeer, _, cx| {
            let peer = dragged.name.clone();
            AppState::apply(cx, |state| state.commit_flow_edge(&peer, edge));
        }))
}
