//! Whole-window and contextual chrome for the agent-connection lifecycle: the
//! pre-connection / unreachable / outdated-build frames rendered in place of
//! the real UI, and an attention bar shown only when action is required.

use gpui::{App, Div, IntoElement, ParentElement, SharedString, Styled, div, px, rgb};
use gpui_component::{
    Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    spinner::Spinner,
    v_flex,
};

use crate::app::menu::OpenConfigFolder;
use crate::ui::theme::{self, ContentWidth, FOOTER_H, Palette, Typography as _};

/// Centered spinner over a muted one-line caption — the quiet "still working"
/// body shared by the pre-connection frame and the scanning state, so the two
/// loading phases render as one continuous frame with only the caption
/// changing. The spinner's repeating animation re-renders the window every
/// frame while mounted, which is fine *because* both loading states are
/// bounded: the connecting frame downgrades to the static
/// [`unreachable_body`] when no snapshot arrives, and the scanning state ends
/// with the agent reporting `Ready` or `Unavailable`.
pub(super) fn loading_body(caption: SharedString, cx: &App) -> Div {
    let pal = theme::palette(cx);
    v_flex()
        .items_center()
        .justify_center()
        .gap_3()
        .child(Spinner::new().large().color(pal.text_muted))
        .child(div().text_body().text_color(pal.text_muted).child(caption))
}

/// Static centered notice — icon, headline, muted caption — for the
/// connection-problem frames. Unlike [`loading_body`] there is deliberately
/// no animation: these frames can stay up indefinitely, and an infinite
/// animation would pin the render loop for as long as they do (the same
/// reasoning as the status dot's fixed glow).
pub(super) fn notice_body(headline: SharedString, caption: SharedString, cx: &App) -> Div {
    let pal = theme::palette(cx);
    v_flex()
        .items_center()
        .justify_center()
        .gap_4()
        .p_8()
        .child(
            Icon::new(IconName::TriangleAlert)
                .size_8()
                .text_color(rgb(theme::STATUS_CONNECTING)),
        )
        .child(div().text_title().child(headline))
        .child(
            div()
                .max_w(ContentWidth::Narrow.rems())
                .text_body()
                .text_center()
                .text_color(pal.text_muted)
                .child(caption),
        )
}

/// Whole-window placeholder shown from window-open until the agent's first
/// IPC snapshot lands — normally a fraction of a second. Deliberately
/// neutral: no chrome, no claims about permissions or devices. If the agent
/// stays unreachable, the IPC client downgrades the link and
/// [`unreachable_body`] replaces this frame.
pub(super) fn connecting_body(cx: &App) -> Div {
    loading_body(
        rust_i18n::t!("agent.connecting_to_the_background_service").into(),
        cx,
    )
    .size_full()
}

/// Whole-window frame once the agent has been unreachable well past startup:
/// the spinner would be a lie at this point. Polling (and the spawn retry)
/// keeps running underneath, and the first snapshot swaps the real UI back in.
pub(super) fn unreachable_body(cx: &App) -> Div {
    notice_body(
        rust_i18n::t!("agent.cant_reach_the_background_service").into(),
        rust_i18n::t!("agent.agent_connection_retry").into(),
        cx,
    )
    .size_full()
}

/// Whole-window frame when the *agent* answered with a newer IPC protocol
/// than this process speaks: the app bundle was updated while this window
/// stayed open, and only a relaunch loads the new GUI. Without this frame the
/// window would keep showing live-looking but frozen state.
pub(super) fn outdated_gui_body(cx: &App) -> Div {
    notice_body(
        rust_i18n::t!("agent.openlogi_was_updated").into(),
        rust_i18n::t!("agent.updated_window_requires_relaunch").into(),
        cx,
    )
    .size_full()
    .child(
        Button::new("relaunch-gui")
            .primary()
            .label(rust_i18n::t!("agent.relaunch_openlogi"))
            .on_click(|_, _, cx| cx.restart()),
    )
}

/// Fail-closed frame for config load/save/conflict/reload failures.
pub(super) fn config_issue_body(message: SharedString, cx: &App) -> Div {
    notice_body(rust_i18n::t!("device.configuration").into(), message, cx)
        .size_full()
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("open-config-folder")
                        .label(rust_i18n::t!("app.open_configuration_folder"))
                        .on_click(|_, _, cx| cx.dispatch_action(&OpenConfigFolder)),
                )
                .child(
                    Button::new("restart-after-config-error")
                        .primary()
                        .label(rust_i18n::t!("agent.relaunch_openlogi"))
                        .on_click(|_, _, cx| cx.restart()),
                ),
        )
}

/// Contextual attention bar. Normal operation has no footer; this row appears
/// only after the user dismissed the permission gate while Accessibility is
/// still unavailable.
pub(super) fn attention_footer(cx: &App) -> impl IntoElement {
    let pal = theme::palette(cx);
    h_flex()
        .h(px(FOOTER_H))
        .flex_shrink_0()
        .w_full()
        .px_5()
        .items_center()
        .border_t_1()
        .border_color(pal.border)
        .child(accessibility_status(pal))
}

/// Accessibility affordance that requests the grant on click (the native
/// prompt + System Settings, via [`super::request_accessibility`]).
#[cfg(target_os = "macos")]
fn accessibility_status(pal: Palette) -> impl IntoElement {
    // Scoped here rather than at module level: these traits' only user is this
    // macOS-gated affordance (`.hover()` + `.on_click()`), so an ungated import
    // would be unused — and a hard error under `-D warnings` — on Linux/Windows.
    use gpui::InteractiveElement as _;
    use gpui_base::Button as BaseButton;

    BaseButton::new("footer-accessibility")
        .accessibility_label(rust_i18n::t!(
            "permissions.accessibility_not_granted_click_to_grant"
        ))
        .flex()
        .gap_2()
        .items_center()
        .text_caption()
        .text_color(pal.text_primary)
        .cursor_pointer()
        .hover(|style| style.text_color(pal.text_muted))
        .focus_visible(|style| style.text_color(pal.text_muted))
        .child(
            div()
                .size_1p5()
                .rounded_full()
                .bg(rgb(theme::STATUS_CONNECTING)),
        )
        .child(div().child(rust_i18n::t!(
            "permissions.accessibility_not_granted_click_to_grant"
        )))
        .on_click(|_, _, cx| super::request_accessibility(cx))
}

#[cfg(not(target_os = "macos"))]
fn accessibility_status(_pal: Palette) -> impl IntoElement {
    div()
}
