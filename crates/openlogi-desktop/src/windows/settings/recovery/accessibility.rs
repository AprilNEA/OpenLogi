//! Semantics and focus visibility for the recovery workflow's scrollable content.

use gpui::{
    AnyElement, App, Div, ElementId, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, Stateful, StatefulInteractiveElement as _, Window,
    accesskit::Role, div,
};

/// Keep a control group visible when keyboard focus enters or moves within it.
#[derive(IntoElement)]
pub(super) struct FocusScroll {
    id: ElementId,
    child: AnyElement,
    reveal: bool,
}

struct FocusState {
    scope: FocusHandle,
    previous: Option<FocusHandle>,
    revealed: bool,
}

impl FocusScroll {
    pub(super) fn new(id: impl Into<ElementId>, child: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            child: child.into_any_element(),
            reveal: false,
        }
    }

    /// Reveal a newly entered confirmation stage, without stealing focus.
    pub(super) fn reveal(mut self, reveal: bool) -> Self {
        self.reveal = reveal;
        self
    }
}

impl RenderOnce for FocusScroll {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, cx| FocusState {
            scope: cx.focus_handle().tab_stop(false),
            previous: None,
            revealed: false,
        });
        let scope = state.read(cx).scope.clone();
        let tracked_scope = scope.clone();
        div()
            .on_children_prepainted(move |bounds, window, cx| {
                let focused = window
                    .focused(cx)
                    .filter(|_| scope.contains_focused(window, cx));
                let changed = state.update(cx, |state, _| {
                    let changed = (state.previous != focused && focused.is_some())
                        || (self.reveal && !state.revealed);
                    state.revealed = self.reveal;
                    state.previous = focused;
                    changed
                });
                if changed && let Some(bounds) = bounds.first() {
                    reveal_after_frame(bounds.dilate(gpui::px(4.)), window, cx);
                }
            })
            .id(self.id)
            .track_focus(&tracked_scope)
            .child(self.child)
    }
}

// GPUI 0.3.4's List retries prepaint on request_autoscroll, but does not
// roll back accessibility nodes. Route the scroll after this frame instead;
// Settings keeps ownership of its list, hit testing, and scroll limits.
fn reveal_after_frame(bounds: gpui::Bounds<gpui::Pixels>, window: &Window, cx: &mut App) {
    let viewport = window.content_mask().bounds;
    let delta = if bounds.top() < viewport.top() {
        viewport.top() - bounds.top()
    } else if bounds.bottom() > viewport.bottom() {
        viewport.bottom() - bounds.bottom()
    } else {
        return;
    };
    let focused = window.focused(cx);
    window.defer(cx, move |window, cx| {
        if window.focused(cx) != focused {
            return;
        }
        window.dispatch_event(
            gpui::PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                position: viewport.center(),
                delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), delta)),
                ..Default::default()
            }),
            cx,
        );
    });
}

/// A visible text fragment with the same content in the accessibility tree.
pub(super) fn text(
    id: impl Into<ElementId>,
    value: impl Into<gpui::SharedString>,
) -> Stateful<Div> {
    let value = value.into();
    div()
        .id(id)
        .role(Role::Label)
        .aria_label(value.clone())
        .child(value)
}

/// Give status and confirmation text the same semantics as their visual alert.
pub(super) fn alert(
    id: &'static str,
    title: Option<gpui::SharedString>,
    message: gpui::SharedString,
    variant: gpui_component::alert::AlertVariant,
) -> Stateful<Div> {
    use gpui::prelude::FluentBuilder as _;
    let label = title.as_ref().map_or_else(
        || message.to_string(),
        |title| format!("{title}. {message}"),
    );
    div().id(id).role(Role::Alert).aria_label(label).child(
        gpui_component::alert::Alert::new("message", message)
            .with_variant(variant)
            .when_some(title, gpui_component::alert::Alert::title),
    )
}
