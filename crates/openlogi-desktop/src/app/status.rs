//! Whole-window and contextual chrome for the agent-connection lifecycle: the
//! pre-connection / outdated-build frames rendered in place of the real UI,
//! the unreachable frame rendered in place of its content, and an attention
//! bar shown only while something needs saying.

use gpui::{App, Div, IntoElement, ParentElement, SharedString, Styled, div, px, rgb};
use gpui_component::{
    Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    spinner::Spinner,
    v_flex,
};
use openlogi_ipc::{AgentStatus, SecureInput};

use crate::app::menu::open_config_folder;
use crate::state::AppState;
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
/// stays unreachable, the IPC client downgrades the link and the Home header
/// over [`unreachable_body`] replaces this frame.
pub(super) fn connecting_body(cx: &App) -> Div {
    loading_body(tr!("agent.connecting_to_the_background_service"), cx).size_full()
}

/// Content frame once the agent has been unreachable well past startup or a
/// loss: a spinner would be a lie at this point. It sits under the Home header,
/// so Settings — where the usual causes are checked — stays reachable even
/// where there is no menu bar. Polling (and the spawn retry) keeps running
/// underneath, Retry cuts its wait short, and the first snapshot swaps the
/// real UI back in. `retrying` while a retry is under way: only then does the
/// frame animate, and the IPC client answers every retry within its
/// unreachable window.
pub(super) fn unreachable_body(retrying: bool, cx: &App) -> Div {
    use gpui::InteractiveElement as _;

    notice_body(
        tr!("agent.cant_reach_the_background_service"),
        unreachable_advice(),
        cx,
    )
    .flex_1()
    .w_full()
    .min_h_0()
    .child(
        h_flex()
            .gap_2()
            .child(
                Button::new("agent-open-settings")
                    .label(tr!("agent.open_settings"))
                    .on_click(|_, _, cx| open_agent_settings(cx)),
            )
            .child(
                Button::new("agent-retry")
                    .primary()
                    .icon(IconName::RotateCw)
                    .label(tr!("agent.retry_now"))
                    .loading(retrying)
                    .debug_selector(|| "agent-retry".into())
                    .on_click(|_, _, cx| AppState::apply(cx, AppState::retry_agent_connection)),
            ),
    )
}

/// What to check before reinstalling: the usual reason the agent cannot run
/// on this platform.
fn unreachable_advice() -> SharedString {
    if cfg!(target_os = "macos") {
        tr!("agent.agent_unreachable_macos")
    } else if cfg!(target_os = "linux") {
        tr!("agent.agent_unreachable_linux")
    } else {
        tr!("agent.agent_unreachable_windows")
    }
}

/// Open Settings where [`unreachable_advice`] points: input-device access on
/// Linux's Permissions page; on macOS the General page, which shows when the
/// login item is switched off.
fn open_agent_settings(cx: &mut App) {
    use crate::windows::settings::{SettingsPage, open_at};

    #[cfg(target_os = "linux")]
    let page = SettingsPage::Permissions;
    #[cfg(not(target_os = "linux"))]
    let page = SettingsPage::General;
    open_at(page, cx);
}

/// Whole-window frame when the *agent* answered with a newer IPC protocol
/// than this process speaks: the app bundle was updated while this window
/// stayed open, and only a relaunch loads the new GUI. Without this frame the
/// window would keep showing live-looking but frozen state.
pub(super) fn outdated_gui_body(cx: &App) -> Div {
    notice_body(
        tr!("agent.openlogi_was_updated"),
        tr!("agent.updated_window_requires_relaunch"),
        cx,
    )
    .size_full()
    .child(
        Button::new("relaunch-gui")
            .primary()
            .label(tr!("agent.relaunch_openlogi"))
            .on_click(|_, _, cx| cx.restart()),
    )
}

/// Fail-closed frame for config load/save/conflict/reload failures.
pub(super) fn config_issue_body(message: SharedString, cx: &App) -> Div {
    use gpui::InteractiveElement as _;

    notice_body(tr!("device.configuration"), message, cx)
        .size_full()
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("open-config-folder")
                        .label(tr!("app.open_configuration_folder"))
                        .debug_selector(|| "open-config-folder".into())
                        .on_click(|_, _, cx| open_config_folder(cx)),
                )
                .child(
                    Button::new("restart-after-config-error")
                        .primary()
                        .label(tr!("agent.relaunch_openlogi"))
                        .on_click(|_, _, cx| cx.restart()),
                ),
        )
}

/// What the attention bar shows. Normal operation shows nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Attention {
    /// The connection to the agent dropped and is being re-established, so
    /// the window shows the agent's last state, not a live one.
    Reconnecting,
    /// The user dismissed the permission gate while Accessibility is still
    /// unavailable.
    Accessibility,
    /// macOS Secure Event Input is hiding key presses from the hook, so the
    /// user's OS-hook key bindings do not fire. Carries the name of the app
    /// macOS attributes it to, when it names one.
    SecureInput(Option<SharedString>),
}

impl Attention {
    /// Decide the attention bar for `status`. `has_key_bindings` is whether
    /// the user bound any key through the OS hook (the Keys tab's F-row).
    ///
    /// Secure Event Input pauses only those bindings: mouse buttons,
    /// scrolling, gestures and HID++-diverted controls keep working. So it
    /// shows only when such a binding exists and the hook that would run it
    /// is installed. A missing Accessibility grant comes first: without it no
    /// binding fires at all.
    pub(super) fn for_status(status: &AgentStatus, has_key_bindings: bool) -> Option<Self> {
        if !status.accessibility_granted {
            return Some(Self::Accessibility);
        }
        match &status.secure_input {
            SecureInput::On(app) if has_key_bindings && status.hook_installed => Some(
                Self::SecureInput(app.as_ref().map(|app| app.display_name.clone().into())),
            ),
            SecureInput::On(_) | SecureInput::Off => None,
        }
    }

    /// Whether adding the first key binding or removing the last one shows
    /// or hides the bar for `status`. Only the root view draws the bar, so it
    /// repaints on binding edits while this holds.
    pub(super) fn depends_on_key_bindings(status: &AgentStatus) -> bool {
        Self::for_status(status, true) != Self::for_status(status, false)
    }
}

/// Contextual attention bar. Normal operation has no footer; this row appears
/// only while an [`Attention`] applies. The reconnecting spinner animates for
/// as long as it shows, which is bounded: the IPC client either reconnects or
/// declares the agent unreachable.
pub(super) fn attention_footer(attention: Attention, cx: &App) -> impl IntoElement {
    let pal = theme::palette(cx);
    h_flex()
        .h(px(FOOTER_H))
        .flex_shrink_0()
        .w_full()
        .px_5()
        .items_center()
        .border_t_1()
        .border_color(pal.border)
        .child(match attention {
            Attention::Reconnecting => reconnecting_status(pal).into_any_element(),
            Attention::Accessibility => accessibility_status(pal).into_any_element(),
            Attention::SecureInput(app) => secure_input_status(app, pal).into_any_element(),
        })
}

/// The window's state is the agent's last, not a live one. Nothing to click:
/// the IPC client is already reconnecting.
fn reconnecting_status(pal: Palette) -> impl IntoElement {
    use gpui::InteractiveElement as _;

    h_flex()
        .gap_2()
        .items_center()
        .text_caption()
        .text_color(pal.text_primary)
        .child(Spinner::new().xsmall().color(pal.text_muted))
        .child(
            div()
                .debug_selector(|| "agent-reconnecting".into())
                .child(tr!("agent.reconnecting_to_the_background_service")),
        )
}

/// Why key bindings are not firing. Nothing to click: Secure Event Input ends
/// when the app that turned it on turns it off.
fn secure_input_status(app: Option<SharedString>, pal: Palette) -> impl IntoElement {
    use gpui::InteractiveElement as _;

    let message = match app {
        Some(app) => tr!("keyboard.secure_input_pauses_remapping_for_app", app => app),
        None => tr!("keyboard.secure_input_pauses_remapping"),
    };
    h_flex()
        .gap_2()
        .items_center()
        .text_caption()
        .text_color(pal.text_primary)
        .child(
            div()
                .size_1p5()
                .rounded_full()
                .bg(rgb(theme::STATUS_CONNECTING)),
        )
        .child(
            div()
                .debug_selector(|| "secure-input-notice".into())
                .child(message),
        )
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
        .accessibility_label(tr!("permissions.accessibility_not_granted_click_to_grant"))
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
        .child(div().child(tr!("permissions.accessibility_not_granted_click_to_grant")))
        .on_click(|_, _, cx| super::request_accessibility(cx))
}

#[cfg(not(target_os = "macos"))]
fn accessibility_status(_pal: Palette) -> impl IntoElement {
    div()
}
