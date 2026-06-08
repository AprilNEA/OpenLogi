//! First-run prompt asking whether to enable the opt-in update check.
//!
//! Update checks default to off (the README's "no telemetry, no auto-update
//! poller" promise). This small window — shown once, gated by
//! [`AppSettings::update_prompt_seen`](openlogi_core::config::AppSettings) —
//! is how a user opts in on first launch. Either choice marks the prompt seen
//! so it never reappears; "Enable" also runs one check immediately.

use gpui::{
    App, BorrowAppContext as _, Context, FontWeight, InteractiveElement, IntoElement,
    ParentElement as _, Render, Size, Styled as _, Subscription, Window, div, px,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_updater::Updater;

use crate::app_menu::{CloseWindow, Minimize, Zoom};
use crate::state::AppState;
use crate::theme;
use crate::windows::{self, AuxWindow};

/// Standalone first-run update-consent window root view.
pub struct UpdateConsentView {
    #[allow(dead_code, reason = "held to keep the appearance observer alive")]
    appearance_obs: Option<Subscription>,
}

impl UpdateConsentView {
    fn new(_: &mut Window, _: &mut Context<Self>) -> Self {
        Self {
            appearance_obs: None,
        }
    }
}

impl AuxWindow for UpdateConsentView {
    fn set_appearance_obs(&mut self, sub: Subscription) {
        self.appearance_obs = Some(sub);
    }
}

/// Open the first-run update-consent window.
pub fn open(cx: &mut App) {
    windows::open_or_focus(
        |reg| &mut reg.update_consent,
        "OpenLogi",
        Size::new(px(380.), px(220.)),
        UpdateConsentView::new,
        cx,
    );
}

/// Persist the user's answer, run one check if they opted in, and close.
fn answer(enabled: bool, window: &mut Window, cx: &mut App) {
    cx.update_global::<AppState, _>(|state, _| state.record_update_consent(enabled));
    if enabled {
        if let Some(updater) = crate::platform::updater::shared(cx) {
            updater.update(cx, Updater::check);
        }
    }
    window.remove_window();
}

impl Render for UpdateConsentView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);

        v_flex()
            .size_full()
            .bg(pal.bg)
            .text_color(pal.text_primary)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            .items_center()
            .justify_center()
            .gap_4()
            .p_6()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr!("Check for updates?")),
            )
            .child(
                div()
                    .max_w(px(320.))
                    .text_sm()
                    .text_center()
                    .text_color(pal.text_muted)
                    .child(tr!(
                        "OpenLogi can check GitHub once per launch for a new version — query \
                         only, no automatic download or telemetry. You can change this anytime \
                         in Settings."
                    )),
            )
            .child(
                h_flex()
                    .gap_3()
                    .pt_2()
                    .child(
                        Button::new("update-consent-decline")
                            .outline()
                            .label(tr!("Not now"))
                            .on_click(|_, window, cx| answer(false, window, cx)),
                    )
                    .child(
                        Button::new("update-consent-accept")
                            .primary()
                            .label(tr!("Enable"))
                            .on_click(|_, window, cx| answer(true, window, cx)),
                    ),
            )
    }
}
