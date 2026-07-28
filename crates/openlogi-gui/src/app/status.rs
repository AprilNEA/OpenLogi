//! Whole-window and footer chrome for the agent-connection lifecycle: the
//! pre-connection / unreachable / outdated-build frames rendered in place of
//! the real UI, and the footer status bar shown once it's up.

use gpui::{AnyElement, Div, IntoElement, ParentElement, SharedString, Styled, div, px, rgb};
use gpui_component::{
    Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    spinner::Spinner,
    v_flex,
};

use crate::theme::{self, FOOTER_H, Palette, Typography as _};

/// Centered spinner over a muted one-line caption — the quiet "still working"
/// body shared by the pre-connection frame and the scanning state, so the two
/// loading phases render as one continuous frame with only the caption
/// changing. The spinner's repeating animation re-renders the window every
/// frame while mounted, which is fine *because* both loading states are
/// bounded: the connecting frame downgrades to the static
/// [`unreachable_body`] when no snapshot arrives, and the scanning state ends
/// with the agent reporting `Ready` or `Unavailable`.
pub(super) fn loading_body(caption: SharedString, pal: Palette) -> Div {
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
pub(super) fn notice_body(headline: SharedString, caption: SharedString, pal: Palette) -> Div {
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
                .max_w(px(440.))
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
pub(super) fn connecting_body(pal: Palette) -> AnyElement {
    loading_body(tr!("Connecting to the background service…"), pal)
        .size_full()
        .into_any_element()
}

/// Whole-window frame once the agent has been unreachable well past startup:
/// the spinner would be a lie at this point. Polling (and the spawn retry)
/// keeps running underneath, and the first snapshot swaps the real UI back in.
pub(super) fn unreachable_body(pal: Palette) -> AnyElement {
    notice_body(
        tr!("Can't reach the background service"),
        tr!("OpenLogi keeps retrying — if this persists, try reinstalling the app."),
        pal,
    )
    .size_full()
    .into_any_element()
}

/// Whole-window frame when the *agent* answered with a newer IPC protocol
/// than this process speaks: the app bundle was updated while this window
/// stayed open, and only a relaunch loads the new GUI. Without this frame the
/// window would keep showing live-looking but frozen state.
pub(super) fn outdated_gui_body(pal: Palette) -> AnyElement {
    notice_body(
        tr!("OpenLogi was updated"),
        tr!("This window is from the previous version — relaunch to finish the update."),
        pal,
    )
    .size_full()
    .child(
        Button::new("relaunch-gui")
            .primary()
            .label(tr!("Relaunch OpenLogi"))
            .on_click(|_, _, cx| cx.restart()),
    )
    .into_any_element()
}

/// Footer status bar: passive state only. Left — the Accessibility-permission
/// indicator; right — the app version. The former actions (Add Device /
/// Settings / About) moved to where they belong: Add Device to the device
/// header's "+", Settings to the right panel's Configuration card and the menu
/// bar (⌘,), About to the menu bar. Keeping operations out of here leaves a
/// genuine status bar — two quiet readouts at the edges, nothing in the middle.
pub(super) fn footer(pal: Palette, granted: bool) -> impl IntoElement {
    h_flex()
        .h(px(FOOTER_H))
        // Fixed chrome — never shrink when a tab body overflows (see `detail_header`).
        .flex_shrink_0()
        .w_full()
        .px_5()
        .gap_4()
        .items_center()
        .justify_between()
        .border_t_1()
        .border_color(pal.border)
        .child({
            #[cfg(target_os = "macos")]
            let el = accessibility_status(pal, granted);
            #[cfg(not(target_os = "macos"))]
            let el = div().into_any_element();
            let _ = granted;
            el
        })
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(concat!("v", env!("CARGO_PKG_VERSION"))),
        )
}

/// Footer Accessibility-permission indicator. Granted → a muted green-dot
/// status; not granted → an amber-dot affordance that requests the grant on
/// click (the native prompt + System Settings, via [`open_accessibility_settings`]).
#[cfg(target_os = "macos")]
fn accessibility_status(pal: Palette, granted: bool) -> AnyElement {
    // Scoped here rather than at module level: these traits' only user is this
    // macOS-gated affordance (`.id()` + `.on_click()`), so an ungated import
    // would be unused — and a hard error under `-D warnings` — on Linux/Windows.
    use gpui::{InteractiveElement as _, StatefulInteractiveElement as _};

    if granted {
        // Reassurance only — kept deliberately quiet: a small dimmed dot and
        // muted text that recede until something is actually wrong.
        h_flex()
            .gap_1p5()
            .items_center()
            .text_caption()
            .text_color(pal.text_muted)
            .child(
                div()
                    .size_1p5()
                    .rounded_full()
                    .bg(rgb(theme::STATUS_CONNECTED)),
            )
            .child(div().child(tr!("Accessibility granted")))
            .into_any_element()
    } else {
        // The state that needs attention — full-strength text, an amber dot,
        // and a click target that requests the grant.
        h_flex()
            .id("footer-accessibility")
            .gap_2()
            .items_center()
            .text_caption()
            .text_color(pal.text_primary)
            .cursor_pointer()
            .child(
                div()
                    .size_1p5()
                    .rounded_full()
                    .bg(rgb(theme::STATUS_CONNECTING)),
            )
            .child(div().child(tr!("Accessibility not granted · click to grant")))
            .on_click(|_, _, cx| super::request_accessibility(cx))
            .into_any_element()
    }
}
