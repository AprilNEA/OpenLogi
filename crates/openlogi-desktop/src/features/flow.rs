//! Per-device Flow tab.
//!
//! A pointing device gets the Logi-style arrangement editor: computer cards
//! drag-snapped onto the four sides of "This computer", each side switching
//! the device to the card's host when the cursor pushes that screen edge.
//! Every other host-switching device gets the follower choice — whether it
//! jumps along when the Flow mouse switches.
//!
//! "This computer" is labeled with the device's live `ChangeHost` slot, read
//! over IPC on tab entry and whenever the device comes back online (which is
//! exactly when the label can have changed — a switch-away takes the device
//! offline here).

use gpui::{
    AnyElement, AppContext as _, Context, InteractiveElement as _, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement as _, Styled, Subscription, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    switch::Switch,
    v_flex,
};
use openlogi_core::config::{FlowConfig, FlowFollow, FlowSide, FlowTriggerMode};
use openlogi_core::device::DeviceKind;
use openlogi_core::hid::{DeviceRoute, HostInfo, WriteError};

use crate::services::ipc::Command;
use crate::state::{AppState, DeviceRecord, StateEvent};
use crate::ui::theme::{self, Palette, SelectableStyle as _, Typography as _};

/// What the panel knows about the device's current ChangeHost slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostInfoStatus {
    /// No read attempted yet (or the device is unreachable).
    Unknown,
    /// A read is in flight.
    Reading,
    /// The device answered.
    Ready(HostInfo),
    /// The read failed; the label falls back to a plain "This computer".
    Failed,
}

pub struct FlowPanel {
    host_info: HostInfoStatus,
    /// The device key + online flag the current `host_info` belongs to — the
    /// stale fence, and the offline→online re-read trigger.
    read_target: Option<(String, bool)>,
    /// Bumped per read so a slow reply can't overwrite a newer device's.
    read_seq: u64,
    _state_obs: Subscription,
}

impl FlowPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = cx.subscribe(
            &AppState::global(cx),
            |_panel, _, event: &StateEvent, cx| {
                if matches!(
                    event,
                    StateEvent::InventoryChanged
                        | StateEvent::DeviceSelected(_)
                        | StateEvent::DeviceConfigChanged(_)
                ) {
                    cx.notify();
                }
            },
        );
        Self {
            host_info: HostInfoStatus::Unknown,
            read_target: None,
            read_seq: 0,
            _state_obs: state_obs,
        }
    }

    /// Keep `host_info` tracking the rendered device: (re)read when the
    /// device changes or comes back online, and reset when it goes away.
    fn ensure_host_info(
        &mut self,
        key: &str,
        online: bool,
        route: Option<DeviceRoute>,
        cx: &mut Context<Self>,
    ) {
        let reachable = online && route.is_some();
        if self
            .read_target
            .as_ref()
            .is_some_and(|(read_key, was_reachable)| {
                read_key == key && (*was_reachable == reachable || !reachable)
            })
        {
            return;
        }
        self.read_target = Some((key.to_string(), reachable));
        let Some(route) = route.filter(|_| reachable) else {
            self.host_info = HostInfoStatus::Unknown;
            return;
        };
        self.host_info = HostInfoStatus::Reading;
        self.read_seq += 1;
        let seq = self.read_seq;
        cx.spawn(async move |panel, cx| {
            let sender = cx.update(|cx| AppState::global(cx).read(cx).ipc_sender());
            let (tx, rx) = tokio::sync::oneshot::channel();
            let result = if sender.send(Command::ReadHostInfo(route, tx)).is_ok() {
                rx.await.unwrap_or(Err(WriteError::AgentUnavailable))
            } else {
                Err(WriteError::AgentUnavailable)
            };
            let _ = panel.update(cx, |panel, cx| {
                if panel.read_seq == seq {
                    panel.host_info = match result {
                        Ok(info) => HostInfoStatus::Ready(info),
                        Err(_) => HostInfoStatus::Failed,
                    };
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

impl Render for FlowPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let Some((key, kind, online, route)) = AppState::try_read(cx)
            .and_then(AppState::current_record)
            .map(|record| {
                (
                    record.config_key.clone(),
                    record.kind,
                    record.online,
                    record.route.clone(),
                )
            })
        else {
            return div().into_any_element();
        };
        let pointing = matches!(
            kind,
            DeviceKind::Mouse | DeviceKind::Trackball | DeviceKind::Touchpad
        );
        if pointing {
            self.ensure_host_info(&key, online, route, cx);
            let flow = AppState::try_read(cx).map(AppState::current_flow);
            // Forget the last read and re-render, which re-triggers
            // `ensure_host_info` — the recovery path for a read that failed
            // while the device was asleep, and a manual re-check after a
            // switch-away-and-back.
            let refresh: RefreshHandler =
                Box::new(cx.listener(|panel, _: &gpui::ClickEvent, _window, cx| {
                    panel.read_target = None;
                    panel.host_info = HostInfoStatus::Unknown;
                    cx.notify();
                }));
            pointer_content(&flow.unwrap_or_default(), self.host_info, refresh, pal)
                .into_any_element()
        } else {
            let follow = AppState::try_read(cx)
                .map(AppState::current_flow_follow)
                .unwrap_or_default();
            let candidates = AppState::try_read(cx)
                .map(AppState::flow_pointer_candidates)
                .unwrap_or_default();
            follower_content(&follow, &candidates, pal).into_any_element()
        }
    }
}

/// The This-computer card's refresh click, wired back to the panel entity.
type RefreshHandler = Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App)>;

/// Commit one Flow edit against the active device and announce the change.
fn update_flow(cx: &mut gpui::App, edit: impl FnOnce(&mut AppState) + 'static) {
    AppState::update(cx, |state, cx| {
        let key = state.current_record().map(DeviceRecord::device_key);
        edit(state);
        if let Some(key) = key {
            cx.emit(StateEvent::DeviceConfigChanged(key));
        }
    });
}

// ---------- pointer (arrangement) content ----------

/// Drag payload: which side's card is being dragged, and its host.
#[derive(Clone)]
struct DragFlowCard {
    from: FlowSide,
    host: u8,
}

/// The floating preview shown under the cursor during a drag.
struct DragCardPreview {
    host: u8,
}

impl Render for DragCardPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        computer_card_shell(pal)
            .opacity(0.9)
            .child(card_title(host_label(self.host), pal))
    }
}

fn pointer_content(
    flow: &FlowConfig,
    host_info: HostInfoStatus,
    refresh: RefreshHandler,
    pal: Palette,
) -> impl IntoElement {
    let enabled = flow.enabled;
    v_flex()
        .w_full()
        .gap_4()
        .child(enable_row(enabled, pal))
        .when(enabled, |this| {
            this.child(section_divider(pal))
                .child(trigger_section(flow.trigger, pal))
                .child(section_divider(pal))
                .child(arrangement_section(flow, host_info, refresh, pal))
        })
}

/// Hairline between the panel's sections.
fn section_divider(pal: Palette) -> gpui::Div {
    div().w_full().h(px(1.)).flex_none().bg(pal.border)
}

/// A section's leading title + wrapped muted description. `flex_1().min_w_0()`
/// is what lets the description wrap instead of pushing its row's trailing
/// control out of the card.
fn section_heading(title: SharedString, description: SharedString, pal: Palette) -> gpui::Div {
    v_flex()
        .flex_1()
        .min_w_0()
        .gap_1()
        .child(div().text_body().child(title))
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(description),
        )
}

fn enable_row(enabled: bool, pal: Palette) -> impl IntoElement {
    h_flex()
        .w_full()
        .justify_between()
        .items_start()
        .gap_6()
        .child(section_heading(
            tr!("Enable Flow"),
            enable_description(),
            pal,
        ))
        .child(
            div()
                .flex_none()
                .pt_1()
                .child(Switch::new("flow-enabled").checked(enabled).on_click(
                    |checked, _window, cx| {
                        let enabled = *checked;
                        update_flow(cx, move |state| state.set_flow_enabled(enabled));
                    },
                )),
        )
}

/// The enable description; Linux copy names the Wayland limitation instead of
/// letting the switch look broken there.
fn enable_description() -> SharedString {
    let base = tr!(
        "Push the cursor against a mapped screen edge to move your mouse and keyboard to another computer they are paired with. Both computers must run OpenLogi."
    );
    if cfg!(target_os = "linux") {
        format!(
            "{base} {}",
            tr!(
                "Flow needs a global cursor position and is unavailable on native Wayland sessions."
            )
        )
        .into()
    } else {
        base
    }
}

/// One house-styled segmented choice: the accent border + tint that marks
/// "selected" everywhere else in the app (see [`SelectableStyle`]), instead
/// of the widget chrome's colorless selected state.
fn choice_chip(
    id: impl Into<gpui::ElementId>,
    label: SharedString,
    selected: bool,
    compact: bool,
    pal: Palette,
    on_click: impl Fn(&mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .map(|this| {
            if compact {
                this.h(px(22.)).px_2().text_caption()
            } else {
                this.h(px(theme::CONTROL_H)).px_3().text_body()
            }
        })
        .rounded(pal.control_radius)
        .cursor_pointer()
        .text_color(pal.text_primary)
        .selected_border(selected, pal)
        .selected_fill(selected)
        .when(!selected, |this| {
            this.hover(|style| style.bg(pal.control_hover))
        })
        .child(label)
        .on_click(move |_event, _window, cx| on_click(cx))
}

/// The trigger choice in its own titled section.
fn trigger_section(trigger: FlowTriggerMode, pal: Palette) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap_2()
        .child(section_heading(
            tr!("Trigger"),
            tr!("Switch as soon as the cursor pushes a mapped edge, or only while a Ctrl key is held."),
            pal,
        ))
        .child(
            h_flex()
                .gap_2()
                .child(choice_chip(
                    "flow-trigger-edge",
                    tr!("Move to edge"),
                    trigger == FlowTriggerMode::Edge,
                    false,
                    pal,
                    |cx| update_flow(cx, |state| state.set_flow_trigger(FlowTriggerMode::Edge)),
                ))
                .child(choice_chip(
                    "flow-trigger-ctrl",
                    tr!("Hold Ctrl and move to edge"),
                    trigger == FlowTriggerMode::CtrlEdge,
                    false,
                    pal,
                    |cx| {
                        update_flow(cx, |state| {
                            state.set_flow_trigger(FlowTriggerMode::CtrlEdge);
                        });
                    },
                )),
        )
}

/// The arrangement editor in its own titled section.
fn arrangement_section(
    flow: &FlowConfig,
    host_info: HostInfoStatus,
    refresh: RefreshHandler,
    pal: Palette,
) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap_3()
        .child(section_heading(
            tr!("Arrangement"),
            format!(
                "{} {}",
                tr!("Drag a computer card to the edge your other screen sits on."),
                tr!("Click an empty edge to add another computer.")
            )
            .into(),
            pal,
        ))
        .child(arrangement_canvas(flow, host_info, refresh, pal))
}

const SLOT_W: f32 = 148.;
const SLOT_H: f32 = 96.;

fn arrangement_canvas(
    flow: &FlowConfig,
    host_info: HostInfoStatus,
    refresh: RefreshHandler,
    pal: Palette,
) -> impl IntoElement {
    let spacer = || div().w(px(SLOT_W)).h(px(SLOT_H));
    v_flex()
        .w_full()
        .items_center()
        .gap_2()
        .child(
            h_flex()
                .gap_2()
                .child(spacer())
                .child(side_slot(FlowSide::Top, flow, host_info, pal))
                .child(spacer()),
        )
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(side_slot(FlowSide::Left, flow, host_info, pal))
                .child(this_computer_card(host_info, refresh, pal))
                .child(side_slot(FlowSide::Right, flow, host_info, pal)),
        )
        .child(
            h_flex()
                .gap_2()
                .child(spacer())
                .child(side_slot(FlowSide::Bottom, flow, host_info, pal))
                .child(spacer()),
        )
}

fn host_count(host_info: HostInfoStatus) -> u8 {
    match host_info {
        HostInfoStatus::Ready(info) => info.host_count.max(1),
        _ => 3,
    }
}

fn current_host(host_info: HostInfoStatus) -> Option<u8> {
    match host_info {
        HostInfoStatus::Ready(info) => Some(info.current_host),
        _ => None,
    }
}

/// Hosts a card may target: every slot except the one this computer
/// occupies. Hosts held by the other card stay selectable — picking one
/// swaps the two cards' hosts (see `AppState::assign_flow_host`).
fn selectable_hosts(host_info: HostInfoStatus) -> Vec<u8> {
    (0..host_count(host_info))
        .filter(|host| Some(*host) != current_host(host_info))
        .collect()
}

fn side_index(side: FlowSide) -> usize {
    FlowSide::ALL
        .iter()
        .position(|&candidate| candidate == side)
        .unwrap_or_default()
}

fn host_label(host: u8) -> SharedString {
    tr!("Host %{number}", number => (u32::from(host) + 1).to_string())
}

fn card_title(label: SharedString, pal: Palette) -> gpui::Div {
    h_flex()
        .gap_2()
        .items_center()
        .justify_center()
        .text_body()
        .text_color(pal.text_primary)
        .child(
            Icon::empty()
                .path("action-icons/monitor.svg")
                .size_4()
                .flex_none(),
        )
        .child(label)
}

fn computer_card_shell(pal: Palette) -> gpui::Div {
    v_flex()
        .w(px(SLOT_W))
        // `min_h`, not `h`: a wrapped host-picker row (three chips while the
        // current host is unknown) grows the card instead of spilling out.
        .min_h(px(SLOT_H))
        .items_center()
        .justify_center()
        .gap_2()
        .py_2()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        // The control fill, not the panel fill the surrounding card already
        // uses — a card must read as a distinct object on the canvas.
        .bg(pal.control)
}

/// The fixed center card, labeled with the live current host when known,
/// with a refresh control that re-reads it — the recovery path when the
/// first read raced a sleeping device, and a manual re-check after switching
/// away and back.
fn this_computer_card(
    host_info: HostInfoStatus,
    refresh: RefreshHandler,
    pal: Palette,
) -> impl IntoElement {
    let subtitle: Option<SharedString> = match host_info {
        HostInfoStatus::Ready(info) => Some(host_label(info.current_host)),
        HostInfoStatus::Reading => Some(tr!("Reading current host…")),
        HostInfoStatus::Failed => Some(tr!("Host unknown")),
        HostInfoStatus::Unknown => None,
    };
    computer_card_shell(pal)
        .border_color(theme::accent())
        .bg(theme::accent_tint())
        .relative()
        .child(
            div().absolute().top_1().right_1().child(
                Button::new("flow-host-refresh")
                    .xsmall()
                    .ghost()
                    .icon(Icon::empty().path("action-icons/refresh-cw.svg"))
                    .on_click(move |event, window, cx| refresh(event, window, cx)),
            ),
        )
        .child(card_title(tr!("This computer"), pal))
        .when_some(subtitle, |this, subtitle| {
            this.child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(subtitle),
            )
        })
}

/// One snap slot: the draggable computer card when the side is mapped, a
/// drop-target placeholder otherwise.
fn side_slot(
    side: FlowSide,
    flow: &FlowConfig,
    host_info: HostInfoStatus,
    pal: Palette,
) -> AnyElement {
    let index = side_index(side);
    match flow.placements.get(side) {
        Some(host) => {
            let hosts = selectable_hosts(host_info);
            computer_card_shell(pal)
                .id(("flow-card", index))
                .cursor_grab()
                .on_drag(
                    DragFlowCard { from: side, host },
                    |drag, _offset, _window, cx| {
                        cx.stop_propagation();
                        let host = drag.host;
                        cx.new(|_| DragCardPreview { host })
                    },
                )
                .drag_over::<DragFlowCard>(|style, _, _, _| style.bg(theme::accent_tint()))
                .on_drop(move |drag: &DragFlowCard, _window, cx| {
                    let from = drag.from;
                    update_flow(cx, move |state| state.move_flow_placement(from, side));
                })
                .relative()
                .child(
                    // The remove control floats in the corner so the title
                    // stays visually centered like "This computer".
                    div().absolute().top_1().right_1().child(
                        Button::new(("flow-card-remove", index))
                            .xsmall()
                            .ghost()
                            .icon(IconName::Close)
                            .on_click(move |_event, _window, cx| {
                                update_flow(cx, move |state| {
                                    state.set_flow_placement(side, None);
                                });
                            }),
                    ),
                )
                .child(card_title(host_label(host), pal))
                .when(!hosts.is_empty(), |this| {
                    this.child(host_picker(side, host, &hosts, pal))
                })
                .into_any_element()
        }
        None => empty_slot(side, index, flow, host_info, pal),
    }
}

/// An unmapped side: always a drop target, and — while another computer can
/// still be added — a click target that places one right there.
fn empty_slot(
    side: FlowSide,
    index: usize,
    flow: &FlowConfig,
    host_info: HostInfoStatus,
    pal: Palette,
) -> AnyElement {
    let base = v_flex()
        .id(("flow-slot", index))
        .w(px(SLOT_W))
        .h(px(SLOT_H))
        .items_center()
        .justify_center()
        .gap_1()
        .rounded(pal.control_radius)
        .border_1()
        .border_dashed()
        .drag_over::<DragFlowCard>(|style, _, _, _| style.bg(theme::accent_tint()))
        .on_drop(move |drag: &DragFlowCard, _window, cx| {
            let from = drag.from;
            update_flow(cx, move |state| state.move_flow_placement(from, side));
        });
    match addable_host(flow, host_info) {
        Some(host) => base
            .border_color(pal.border)
            .cursor_pointer()
            .hover(|style| style.bg(pal.control_hover))
            .text_color(pal.text_muted)
            .child(Icon::new(IconName::Plus).size_4())
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("Add a computer")),
            )
            .on_click(move |_event, _window, cx| {
                update_flow(cx, move |state| {
                    state.set_flow_placement(side, Some(host));
                });
            })
            .into_any_element(),
        // Every other computer is already placed: keep the slot as a quiet
        // drop target for rearranging.
        None => base.border_color(pal.muted).into_any_element(),
    }
}

/// The host a newly added computer card would target: the lowest slot no card
/// uses yet, excluding this computer's — or `None` once every addable
/// computer is placed (up to two others, fewer on a two-host device).
fn addable_host(flow: &FlowConfig, host_info: HostInfoStatus) -> Option<u8> {
    let max_others = usize::from(host_count(host_info).saturating_sub(1)).min(2);
    if flow.placements.len() >= max_others {
        return None;
    }
    (0..host_count(host_info))
        .filter(|host| Some(*host) != current_host(host_info))
        .find(|host| {
            !flow
                .placements
                .iter()
                .any(|(_, occupied)| occupied == *host)
        })
}

/// The card's host choice, labeled one-based like the host keys on the
/// devices themselves. Picking a host the other card holds swaps the two.
fn host_picker(side: FlowSide, selected: u8, hosts: &[u8], pal: Palette) -> impl IntoElement {
    let index = side_index(side);
    h_flex()
        .max_w_full()
        .px_1()
        .flex_wrap()
        .justify_center()
        .gap_1()
        .children(hosts.iter().map(|&host| {
            choice_chip(
                ("flow-card-host", index * 8 + usize::from(host)),
                // Bare one-based numbers: the card title already says "Host",
                // and three "Host N" chips don't fit the card.
                (u32::from(host) + 1).to_string().into(),
                host == selected,
                true,
                pal,
                move |cx| update_flow(cx, move |state| state.assign_flow_host(side, host)),
            )
        }))
}

// ---------- follower content ----------

fn follower_content(
    follow: &FlowFollow,
    candidates: &[(String, String)],
    pal: Palette,
) -> impl IntoElement {
    let selected_key = match follow {
        FlowFollow::Device(key) => Some(key.clone()),
        FlowFollow::Auto | FlowFollow::Off => None,
    };
    v_flex()
        .w_full()
        .gap_3()
        .child(section_heading(
            tr!("Follow the mouse"),
            tr!("When the mouse switches to another computer with Flow, this device follows it."),
            pal,
        ))
        .child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(choice_chip(
                    "flow-follow-auto",
                    tr!("Automatic"),
                    matches!(follow, FlowFollow::Auto),
                    false,
                    pal,
                    |cx| update_flow(cx, |state| state.set_flow_follow(FlowFollow::Auto)),
                ))
                .child(choice_chip(
                    "flow-follow-off",
                    tr!("Don't follow"),
                    matches!(follow, FlowFollow::Off),
                    false,
                    pal,
                    |cx| update_flow(cx, |state| state.set_flow_follow(FlowFollow::Off)),
                ))
                .children(candidates.iter().enumerate().map(|(index, (key, name))| {
                    let target = key.clone();
                    choice_chip(
                        ("flow-follow-device", index),
                        tr!("Follow %{name}", name => name),
                        selected_key.as_deref() == Some(key.as_str()),
                        false,
                        pal,
                        move |cx| {
                            let target = target.clone();
                            update_flow(cx, move |state| {
                                state.set_flow_follow(FlowFollow::Device(target));
                            });
                        },
                    )
                })),
        )
}
