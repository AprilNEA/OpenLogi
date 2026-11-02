//! The Add Computer dialog: open Flow pairing, pick a nearby computer or
//! enter an address, compare the code both computers show, and close once the
//! agent has saved the new computer.
//!
//! The agent owns the pairing session and reports where it stands in each
//! snapshot ([`AppState::flow_pairing`]); the dialog draws that phase and
//! keeps only what the user picks or types, in [`PairingForm`]. That form is
//! its own entity because the dialog is built while [`FlowView`] renders, so
//! the builder cannot read the view. Pairing is open exactly while the dialog
//! is: closing it, or the window, stops pairing.

use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, Subscription, WeakEntity, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::Dialog,
    h_flex,
    input::{InputEvent, InputState},
    spinner::Spinner,
    v_flex,
};
use openlogi_core::config::FlowPublicKey;
use openlogi_ipc::{
    FlowPairTarget, FlowPairingCommandError, FlowPairingFailure, FlowPairingPeer, FlowPairingPhase,
    FlowPlatform,
};

use super::FlowView;
use crate::state::{AppState, FlowHost, FlowPairingRefusal};
use crate::ui::components::control_input;
use crate::ui::theme::{self, Palette, Typography as _};

/// What the dialog keeps while it is open: the user's own choices.
pub(super) struct PairingForm {
    /// The nearby computer picked in the list.
    chosen: Option<FlowPublicKey>,
    address: Entity<InputState>,
    /// Whether the address field shows. While it shows and holds text, the
    /// address is what Pair pairs with.
    address_open: bool,
    /// Whether pairing has opened since the dialog asked for it, so that no
    /// phase means the agent closed it rather than that it is still opening.
    opened: bool,
    /// Takes keyboard focus back when what had it went away with a step, so
    /// Escape still reaches the dialog.
    focus: FocusHandle,
    _address_events: Subscription,
    _focus_lost: Subscription,
}

impl PairingForm {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let address = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr!("flow.pair_address_placeholder"))
        });
        let address_events = cx.subscribe_in(
            &address,
            window,
            |form, _, event: &InputEvent, _, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => {
                    if let Some(address) = form.address(cx) {
                        AppState::apply(cx, |state| {
                            state.pair_flow_peer(FlowPairTarget::Address(address))
                        });
                    }
                }
                _ => {}
            },
        );
        let focus_lost = cx.on_focus_lost(window, |form, window, cx| form.focus.focus(window, cx));
        Self {
            chosen: None,
            address,
            address_open: false,
            opened: false,
            focus: cx.focus_handle(),
            _address_events: address_events,
            _focus_lost: focus_lost,
        }
    }

    fn address(&self, cx: &App) -> Option<String> {
        let address = self.address.read(cx).value().trim().to_owned();
        (self.address_open && !address.is_empty()).then_some(address)
    }
}

impl FlowView {
    /// Open the Add Computer dialog and open pairing with it.
    pub(super) fn open_pairing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pairing.is_some() {
            return;
        }
        let form = cx.new(|cx| PairingForm::new(window, cx));
        self.pairing = Some(form.clone());
        AppState::apply(cx, AppState::start_flow_pairing);
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| build(dialog, &form, &view, cx));
    }

    /// Follow the agent's pairing phase while the dialog is open: note that
    /// pairing opened, and once the new computer is saved, close the dialog
    /// and select the computer, which waits for a side.
    pub(super) fn follow_pairing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.pairing.clone() else {
            return;
        };
        let phase = AppState::try_read(cx).and_then(|state| state.flow_pairing().cloned());
        match phase {
            Some(FlowPairingPhase::Paired { name, .. }) => {
                self.close_pairing(window, cx);
                self.select(FlowHost::Peer(name), cx);
            }
            Some(_) => form.update(cx, |form, _| form.opened = true),
            None => {}
        }
    }

    /// Close the dialog and stop pairing.
    fn close_pairing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pairing.is_some() {
            window.close_dialog(cx);
            self.end_pairing(cx);
        }
    }

    /// Stop pairing once the dialog is gone, however it went.
    pub(super) fn end_pairing(&mut self, cx: &mut App) {
        if self.pairing.take().is_some() {
            AppState::apply(cx, AppState::stop_flow_pairing);
        }
    }
}

/// One computer in the nearby list.
struct Nearby {
    peer: FlowPairingPeer,
    /// Already one of this computer's peers.
    added: bool,
}

/// What the dialog shows, read from the agent's phase and the form.
enum Step {
    /// Pairing is open or opening: pick a computer.
    Choose {
        nearby: Vec<Nearby>,
        /// What Pair pairs with now.
        target: Option<FlowPairTarget>,
        refusal: Option<SharedString>,
    },
    /// The computers are connecting.
    Connecting(Option<FlowPairingPeer>),
    /// Both computers show `code`; `confirmed` once this one said it matches.
    Code {
        peer: FlowPairingPeer,
        code: String,
        confirmed: bool,
        refusal: Option<SharedString>,
    },
    /// Saved; the window closes the dialog on this phase.
    Saved,
    Failed {
        peer: Option<FlowPairingPeer>,
        reason: FlowPairingFailure,
    },
    /// Pairing is not open: the agent closed it or could not open it.
    Closed(SharedString),
}

impl Step {
    fn read(form: &PairingForm, cx: &App) -> Self {
        let state = AppState::try_read(cx);
        let refusal = state.and_then(AppState::flow_pairing_refusal);
        let phase = state.and_then(AppState::flow_pairing);
        let refused = refusal.and_then(refusal_text);
        let choose = |found: &[FlowPairingPeer]| {
            let peers = state.map(|state| state.flow_config().peers());
            let nearby: Vec<Nearby> = found
                .iter()
                .map(|peer| Nearby {
                    added: peers.is_some_and(|peers| {
                        peers
                            .iter()
                            .any(|known| known.public_key == peer.public_key)
                    }),
                    peer: peer.clone(),
                })
                .collect();
            let target = form.address(cx).map(FlowPairTarget::Address).or_else(|| {
                let chosen = form.chosen?;
                nearby
                    .iter()
                    .any(|nearby| !nearby.added && nearby.peer.public_key == chosen)
                    .then_some(FlowPairTarget::Nearby(chosen))
            });
            Step::Choose {
                nearby,
                target,
                refusal: refused.clone(),
            }
        };
        match phase {
            None => match refusal {
                Some(
                    FlowPairingRefusal::AgentUnreachable
                    | FlowPairingRefusal::Refused(FlowPairingCommandError::Unavailable { .. }),
                ) => Step::Closed(refused.unwrap_or_default()),
                _ if form.opened => Step::Closed(tr!("flow.pair_closed")),
                _ => choose(&[]),
            },
            Some(FlowPairingPhase::Searching) => choose(&[]),
            Some(FlowPairingPhase::Found(found)) => choose(found),
            Some(FlowPairingPhase::Connecting { peer }) => Step::Connecting(peer.clone()),
            Some(FlowPairingPhase::ShowingCode { peer, code }) => Step::Code {
                peer: peer.clone(),
                code: code.clone(),
                confirmed: false,
                refusal: refused,
            },
            Some(FlowPairingPhase::WaitingForPeer { peer, code }) => Step::Code {
                peer: peer.clone(),
                code: code.clone(),
                confirmed: true,
                refusal: refused,
            },
            Some(FlowPairingPhase::Paired { .. }) => Step::Saved,
            Some(FlowPairingPhase::Failed { peer, reason }) => Step::Failed {
                peer: peer.clone(),
                reason: reason.clone(),
            },
        }
    }

    /// No attempt runs, so a click outside can close the dialog without
    /// cancelling one.
    fn is_idle(&self) -> bool {
        matches!(
            self,
            Step::Choose { .. } | Step::Failed { .. } | Step::Closed(_)
        )
    }
}

fn build(
    dialog: Dialog,
    form: &Entity<PairingForm>,
    view: &WeakEntity<FlowView>,
    cx: &mut App,
) -> Dialog {
    let step = Step::read(form.read(cx), cx);
    let focus = form.read(cx).focus.clone();
    let pal = theme::palette(cx);

    let title = match &step {
        Step::Code { .. } => tr!("flow.pair_code_title"),
        Step::Failed {
            peer: Some(peer), ..
        } => tr!("flow.pair_failed_title", name => display_name(peer).to_string()),
        Step::Failed { peer: None, .. } => tr!("flow.pair_failed_title_anyone"),
        _ => tr!("flow.pair_title"),
    };
    let body = match &step {
        Step::Choose {
            nearby,
            target,
            refusal,
        } => choose_body(
            nearby,
            !matches!(target, Some(FlowPairTarget::Address(_))),
            refusal.clone(),
            pal,
            form,
            cx,
        )
        .into_any_element(),
        Step::Connecting(peer) => busy_line(
            peer.as_ref().map_or_else(
                || tr!("flow.pair_connecting_anyone"),
                |peer| tr!("flow.pair_connecting", name => display_name(peer).to_string()),
            ),
            pal,
        )
        .into_any_element(),
        Step::Code {
            peer,
            code,
            confirmed,
            refusal,
        } => code_body(peer, code, *confirmed, refusal.clone(), pal, cx).into_any_element(),
        Step::Saved => div().into_any_element(),
        Step::Failed { peer, reason } => failed_body(peer.as_ref(), reason, pal).into_any_element(),
        Step::Closed(reason) => caption(reason.clone(), pal).into_any_element(),
    };
    let on_close = view.clone();
    dialog
        .w(px(440.))
        .overlay_closable(step.is_idle())
        .title(title)
        .child(
            div()
                .debug_selector(|| "flow-pair".into())
                .track_focus(&focus)
                .child(body),
        )
        .footer(footer(&step, form, view))
        // Enter would otherwise close the dialog as confirmed, stopping
        // pairing; each step's buttons say what Enter could mean.
        .on_ok(|_, _, _| false)
        .on_close(move |_, _, cx| {
            on_close.update(cx, |view, cx| view.end_pairing(cx)).ok();
        })
}

/// `list_chosen` is false while a typed address, not the list, is what Pair
/// pairs with.
fn choose_body(
    nearby: &[Nearby],
    list_chosen: bool,
    refusal: Option<SharedString>,
    pal: Palette,
    form: &Entity<PairingForm>,
    cx: &App,
) -> impl IntoElement {
    let state = form.read(cx);
    let chosen = state.chosen.filter(|_| list_chosen);
    let address_open = state.address_open;
    let address = state.address.clone();
    let searching = if nearby.is_empty() {
        tr!("flow.pair_searching")
    } else {
        tr!("flow.pair_still_searching")
    };
    let toggle = {
        let form = form.clone();
        Button::new("flow-pair-address-toggle")
            .ghost()
            .xsmall()
            .icon(if address_open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .label(tr!("flow.pair_by_address"))
            .debug_selector(|| "flow-pair-address-toggle".into())
            .on_click(move |_, window, cx| {
                form.update(cx, |form, cx| {
                    form.address_open = !form.address_open;
                    if form.address_open {
                        form.address.update(cx, |input, cx| input.focus(window, cx));
                    }
                    cx.notify();
                });
            })
    };
    v_flex()
        .gap_4()
        .child(caption(tr!("flow.pair_intro"), pal))
        .child(
            v_flex()
                .gap_2()
                .child(div().text_subheading().child(tr!("flow.pair_nearby")))
                .children(nearby.iter().map(|nearby| {
                    nearby_row(nearby, chosen == Some(nearby.peer.public_key), pal, form)
                }))
                .child(busy_line(searching, pal)),
        )
        .child(
            v_flex()
                .gap_2()
                .items_start()
                .child(toggle)
                .when(address_open, |section| {
                    section
                        .child(
                            div()
                                .w_full()
                                .debug_selector(|| "flow-pair-address".into())
                                .child(control_input(&address)),
                        )
                        .child(caption(tr!("flow.pair_address_hint"), pal))
                }),
        )
        .when_some(refusal, |body, refusal| body.child(problem(refusal, cx)))
}

fn nearby_row(
    nearby: &Nearby,
    chosen: bool,
    pal: Palette,
    form: &Entity<PairingForm>,
) -> impl IntoElement {
    let name = display_name(&nearby.peer);
    let detail = if nearby.added {
        Some(tr!("flow.pair_already_added"))
    } else {
        platform_label(nearby.peer.platform).map(SharedString::from)
    };
    let selector = format!("flow-pair-peer-{}", nearby.peer.public_key);
    let key = nearby.peer.public_key;
    let form = form.clone();
    BaseButton::new(SharedString::from(selector.clone()))
        .accessibility_label(name.clone())
        .debug_selector(move || selector.clone())
        .selected(chosen)
        .disabled(nearby.added)
        .on_click(move |_, _, cx| {
            form.update(cx, |form, cx| {
                form.chosen = Some(key);
                // A pick in the list is what Pair pairs with now.
                form.address_open = false;
                cx.notify();
            });
        })
        .w_full()
        .px_3()
        .py_2()
        .flex()
        .items_center()
        .gap_3()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(if chosen { theme::accent() } else { pal.border })
        .when(chosen, |row| row.bg(theme::accent_tint()))
        .when(!nearby.added, |row| {
            row.cursor_pointer()
                .hover(|style| style.bg(pal.control_hover))
        })
        .when(nearby.added, |row| row.opacity(0.6))
        .child(
            Icon::empty()
                .path("action-icons/monitor.svg")
                .size_4()
                .flex_none()
                .text_color(pal.text_muted),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .items_start()
                .child(div().text_body().truncate().child(name))
                .when_some(detail, |col, detail| col.child(caption(detail, pal))),
        )
        .when(chosen, |row| {
            row.child(
                Icon::new(IconName::Check)
                    .size_4()
                    .flex_none()
                    .text_color(theme::accent()),
            )
        })
}

fn code_body(
    peer: &FlowPairingPeer,
    code: &str,
    confirmed: bool,
    refusal: Option<SharedString>,
    pal: Palette,
    cx: &App,
) -> impl IntoElement {
    let name = display_name(peer).to_string();
    v_flex()
        .gap_4()
        .child(
            div()
                .text_body()
                .child(tr!("flow.pair_code_body", name => name.clone())),
        )
        .child(
            div()
                .debug_selector(|| "flow-pair-code".into())
                .w_full()
                .py_3()
                .rounded(pal.card_radius)
                .bg(pal.muted)
                .text_center()
                .text_size(px(34.))
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(if confirmed {
                    pal.text_muted
                } else {
                    pal.text_primary
                })
                .child(SharedString::from(grouped(code))),
        )
        .child(if confirmed {
            busy_line(tr!("flow.pair_waiting", name => name), pal).into_any_element()
        } else {
            caption(tr!("flow.pair_code_hint"), pal).into_any_element()
        })
        .when_some(refusal, |body, refusal| body.child(problem(refusal, cx)))
}

fn failed_body(
    peer: Option<&FlowPairingPeer>,
    reason: &FlowPairingFailure,
    pal: Palette,
) -> impl IntoElement {
    let name = peer
        .map_or_else(|| tr!("flow.pair_unnamed"), display_name)
        .to_string();
    let text = match reason {
        FlowPairingFailure::PeerCancelled => {
            tr!("flow.pair_failed_cancelled", name => name.clone())
        }
        FlowPairingFailure::CodeMismatch => tr!("flow.pair_failed_mismatch", name => name.clone()),
        FlowPairingFailure::TimedOut => tr!("flow.pair_failed_timed_out"),
        FlowPairingFailure::Unreachable => tr!("flow.pair_failed_unreachable"),
        FlowPairingFailure::PeerBusy => tr!("flow.pair_failed_busy", name => name.clone()),
        FlowPairingFailure::IncompatibleVersion => tr!("flow.pair_failed_version"),
        FlowPairingFailure::ConnectionLost => tr!("flow.pair_failed_connection_lost"),
        FlowPairingFailure::SaveFailed { message } => {
            tr!("flow.pair_failed_save", message => message.clone())
        }
    };
    // Both of these can end the attempt after the other computer saved this
    // one: it confirmed last, or this computer could not save.
    let other_side = matches!(
        reason,
        FlowPairingFailure::ConnectionLost | FlowPairingFailure::SaveFailed { .. }
    );
    v_flex()
        .gap_2()
        .child(div().text_body().child(text))
        .when(other_side, |body| {
            body.child(caption(
                tr!("flow.pair_failed_other_side", name => name),
                pal,
            ))
        })
}

fn footer(
    step: &Step,
    form: &Entity<PairingForm>,
    view: &WeakEntity<FlowView>,
) -> impl IntoElement {
    let close = |id: &'static str, label: SharedString| {
        let view = view.clone();
        Button::new(id)
            .outline()
            .label(label)
            .debug_selector(move || id.into())
            .on_click(move |_, window, cx| {
                view.update(cx, |view, cx| view.close_pairing(window, cx))
                    .ok();
            })
    };
    let state_button =
        |id: &'static str,
         label: SharedString,
         primary: bool,
         act: fn(&mut AppState) -> crate::state::StateEvents| {
            Button::new(id)
                .when(primary, Button::primary)
                .when(!primary, Button::outline)
                .label(label)
                .debug_selector(move || id.into())
                .on_click(move |_, _, cx| AppState::apply(cx, act))
        };
    let buttons: Vec<gpui::AnyElement> = match step {
        Step::Choose { target, .. } => {
            let target = target.clone();
            vec![
                close("flow-pair-close", tr!("common.cancel")).into_any_element(),
                Button::new("flow-pair-continue")
                    .primary()
                    .label(tr!("flow.pair"))
                    .debug_selector(|| "flow-pair-continue".into())
                    .disabled(target.is_none())
                    .on_click(move |_, _, cx| {
                        if let Some(target) = target.clone() {
                            AppState::apply(cx, |state| state.pair_flow_peer(target));
                        }
                    })
                    .into_any_element(),
            ]
        }
        Step::Connecting(_)
        | Step::Code {
            confirmed: true, ..
        } => vec![
            state_button(
                "flow-pair-cancel",
                tr!("flow.pair_cancel"),
                false,
                AppState::cancel_flow_pairing,
            )
            .into_any_element(),
        ],
        Step::Code { .. } => vec![
            state_button(
                "flow-pair-cancel",
                tr!("flow.pair_cancel"),
                false,
                AppState::cancel_flow_pairing,
            )
            .into_any_element(),
            state_button(
                "flow-pair-confirm",
                tr!("flow.pair_codes_match"),
                true,
                AppState::confirm_flow_pairing,
            )
            .into_any_element(),
        ],
        Step::Saved => Vec::new(),
        Step::Failed { .. } => vec![
            close("flow-pair-close", tr!("flow.pair_close")).into_any_element(),
            // Dismissing the result shows the nearby computers again.
            state_button(
                "flow-pair-retry",
                tr!("flow.pair_try_again"),
                true,
                AppState::cancel_flow_pairing,
            )
            .into_any_element(),
        ],
        Step::Closed(_) => {
            let form = form.clone();
            vec![
                close("flow-pair-close", tr!("flow.pair_close")).into_any_element(),
                Button::new("flow-pair-search-again")
                    .primary()
                    .label(tr!("flow.pair_search_again"))
                    .debug_selector(|| "flow-pair-search-again".into())
                    .on_click(move |_, _, cx| {
                        form.update(cx, |form, cx| {
                            form.opened = false;
                            cx.notify();
                        });
                        AppState::apply(cx, AppState::start_flow_pairing);
                    })
                    .into_any_element(),
            ]
        }
    };
    h_flex().w_full().justify_end().gap_2().children(buttons)
}

/// A line for a phase that waits on the other computer or the network, with
/// a spinner so the wait does not read as a hang.
fn busy_line(text: SharedString, pal: Palette) -> impl IntoElement {
    h_flex()
        .gap_2()
        .items_center()
        .text_caption()
        .text_color(pal.text_muted)
        .child(Spinner::new().small().color(pal.text_muted))
        .child(text)
}

fn caption(text: SharedString, pal: Palette) -> impl IntoElement {
    div().text_caption().text_color(pal.text_muted).child(text)
}

/// Why the last command did nothing, next to the controls that sent it.
fn problem(text: SharedString, cx: &App) -> impl IntoElement {
    div()
        .debug_selector(|| "flow-pair-problem".into())
        .text_caption()
        .text_color(cx.theme().danger)
        .child(text)
}

/// The refusal worth saying. A refusal the snapshot already explains says
/// nothing: pairing closed, or the code it would confirm is gone.
fn refusal_text(refusal: &FlowPairingRefusal) -> Option<SharedString> {
    match refusal {
        FlowPairingRefusal::AgentUnreachable => Some(tr!("flow.pair_refused_unreachable")),
        FlowPairingRefusal::Refused(FlowPairingCommandError::Unavailable { message }) => {
            Some(tr!("flow.pair_refused_unavailable", message => message.clone()))
        }
        FlowPairingRefusal::Refused(FlowPairingCommandError::Busy) => {
            Some(tr!("flow.pair_refused_busy"))
        }
        FlowPairingRefusal::Refused(FlowPairingCommandError::UnknownPeer) => {
            Some(tr!("flow.pair_refused_unknown_peer"))
        }
        FlowPairingRefusal::Refused(
            FlowPairingCommandError::NotStarted | FlowPairingCommandError::NothingToConfirm,
        ) => None,
    }
}

/// The name a computer gives itself, or a stand-in when it gives none.
fn display_name(peer: &FlowPairingPeer) -> SharedString {
    if peer.name.trim().is_empty() {
        tr!("flow.pair_unnamed")
    } else {
        SharedString::from(peer.name.clone())
    }
}

fn platform_label(platform: FlowPlatform) -> Option<&'static str> {
    match platform {
        FlowPlatform::Macos => Some("macOS"),
        FlowPlatform::Windows => Some("Windows"),
        FlowPlatform::Linux => Some("Linux"),
        FlowPlatform::Unknown => None,
    }
}

/// Six digits as two groups of three, the way both computers show them.
fn grouped(code: &str) -> String {
    if code.len() == 6 && code.is_ascii() {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_owned()
    }
}
