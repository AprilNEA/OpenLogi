//! First-run prompt asking whether to enable the opt-in update check.
//!
//! Update checks default to off (the README's "no telemetry, no auto-update
//! poller" promise). This small window — shown once, gated by
//! [`AppSettings::update_prompt_seen`](openlogi_core::config::AppSettings) —
//! is how a user opts in on first launch. Either choice marks the prompt seen
//! so it never reappears; "Enable" also runs one check immediately.

use crate::ui::theme::Typography as _;
use gpui::{
    App, Context, FocusHandle, InteractiveElement, IntoElement, ParentElement as _, Render, Size,
    Styled as _, Subscription, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{button::ButtonVariants as _, h_flex, scroll::ScrollableElement as _, v_flex};
use gpui_updater::Updater;

use crate::app::menu::{CloseWindow, Minimize, Zoom};
use crate::state::AppState;
use crate::ui::components::control_button;
use crate::ui::theme;
use crate::windows::{self, AuxWindow};

/// Standalone first-run update-consent window root view.
pub struct UpdateConsentView {
    focus_handle: FocusHandle,
    appearance_obs: Option<Subscription>,
}

impl UpdateConsentView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        Self {
            focus_handle,
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
        Size::new(px(380.), px(320.)),
        UpdateConsentView::new,
        cx,
    );
}

/// Persist the user's answer, run one check if they opted in, and close.
fn answer(enabled: bool, window: &mut Window, cx: &mut App) {
    AppState::apply(cx, |state| state.record_update_consent(enabled));
    if enabled && let Some(updater) = crate::platform::updater::shared(cx) {
        updater.update(cx, Updater::check);
    }
    window.remove_window();
}

impl Render for UpdateConsentView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        theme::apply_ui_scale(window, cx);
        let pal = theme::palette(cx);

        v_flex()
            .size_full()
            .bg(pal.page)
            .text_color(pal.text_primary)
            .track_focus(&self.focus_handle)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            // Linux only: a client-side titlebar at the top of the window; the
            // centred content sits in the flex-column below it. macOS / Windows
            // keep their native titlebar.
            .when(cfg!(target_os = "linux"), |this| {
                this.child(windows::aux_title_bar(tr!("app.openlogi"), cx))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .debug_selector(|| "update-consent-body".into())
                    .overflow_y_scrollbar()
                    .id("update-consent-scroll")
                    .child(
                        v_flex()
                            .w_full()
                            .items_center()
                            .gap_4()
                            .p_6()
                            .child(
                                div()
                                    .text_heading()
                                    .text_center()
                                    .child(tr!("updates.check_for_updates_consent_title")),
                            )
                            .child(
                                div()
                                    .max_w(px(320.))
                                    .text_body()
                                    .text_center()
                                    .text_color(pal.text_muted)
                                    .debug_selector(|| "update-consent-description".into())
                                    .child(tr!("updates.update_consent_description")),
                            ),
                    ),
            )
            // Keep both answers outside the scroller: translations and larger
            // interface scales must not push consent controls out of reach.
            .child(
                h_flex()
                    .flex_shrink_0()
                    .flex_wrap()
                    .justify_center()
                    .gap_3()
                    .px_6()
                    .pb_6()
                    .pt_2()
                    .child(
                        control_button("update-consent-decline")
                            .outline()
                            .label(tr!("common.not_now"))
                            .debug_selector(|| "update-consent-decline".into())
                            .on_click(|_, window, cx| answer(false, window, cx)),
                    )
                    .child(
                        control_button("update-consent-accept")
                            .primary()
                            .label(tr!("common.enable"))
                            .debug_selector(|| "update-consent-accept".into())
                            .on_click(|_, window, cx| answer(true, window, cx)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{
        AppContext as _, Bounds, Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext,
        VisualTestContext, point, size,
    };
    use openlogi_core::config::{Config, UiScale};

    use super::*;
    use crate::services::{assets::AssetResolver, i18n::LOCALE_LOCK};
    use crate::state::ConfigPersistence;

    #[gpui::test]
    fn consent_actions_stay_visible_while_long_copy_scrolls(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            theme::register_builtin_themes(cx);
        });

        for (locale, scale, height, enabled) in [
            ("en", UiScale::Normal, 320., false),
            ("de", UiScale::ExtraLarge, 320., true),
            ("de", UiScale::ExtraLarge, 220., false),
        ] {
            rust_i18n::set_locale(locale);
            let handle = cx.update(|cx| {
                let mut config = Config::ephemeral();
                config.app_settings.ui_scale = scale;
                let (commands, _) = tokio::sync::mpsc::unbounded_channel();
                let state = cx.new(|_| {
                    AppState::with_runtime(
                        config,
                        &[],
                        &[],
                        &AssetResolver::new(),
                        &[],
                        ConfigPersistence::MemoryOnly,
                        commands,
                    )
                });
                AppState::set_global(state, cx);
                open(cx);
                cx.global::<windows::WindowRegistry>()
                    .update_consent
                    .unwrap()
            });
            let mut visual = VisualTestContext::from_window(handle.into(), cx);
            visual.simulate_resize(size(px(380.), px(height)));
            visual.update(|window, cx| window.draw(cx).clear(cx));

            let viewport = Bounds::new(point(px(0.), px(0.)), size(px(380.), px(height)));
            let decline = visual.debug_bounds("update-consent-decline").unwrap();
            let accept = visual.debug_bounds("update-consent-accept").unwrap();
            for button in [decline, accept] {
                assert!(
                    viewport.contains(&button.origin) && viewport.contains(&button.bottom_right()),
                    "{locale} {scale:?} at {height}px: button {button:?} must fit in {viewport:?}"
                );
                assert!(button.size.height >= px(30.));
            }

            if height < 320. {
                let body = visual.debug_bounds("update-consent-body").unwrap();
                let before = visual.debug_bounds("update-consent-description").unwrap();
                visual.simulate_event(ScrollWheelEvent {
                    // ScrollableElement gives the source div its content height;
                    // its centre can be below the clipped viewport and footer.
                    position: body.origin + point(px(20.), px(20.)),
                    delta: ScrollDelta::Pixels(point(px(0.), px(-1000.))),
                    ..Default::default()
                });
                visual.update(|window, cx| window.draw(cx).clear(cx));
                let after = visual.debug_bounds("update-consent-description").unwrap();
                assert!(
                    after.origin.y < before.origin.y,
                    "long copy must scroll in {body:?}: {before:?} -> {after:?}"
                );
                assert!(after.bottom() <= decline.top(), "the end must be reachable");
                assert_eq!(visual.debug_bounds("update-consent-decline"), Some(decline));
                assert_eq!(visual.debug_bounds("update-consent-accept"), Some(accept));
            }

            let button = if enabled { accept } else { decline };
            visual.simulate_click(button.center(), Modifiers::default());
            cx.update(|cx| {
                let state = AppState::try_read(cx).unwrap();
                assert!(state.app_settings().update_prompt_seen);
                assert_eq!(state.app_settings().check_for_updates, enabled);
            });
        }
        rust_i18n::set_locale("en");
    }
}
