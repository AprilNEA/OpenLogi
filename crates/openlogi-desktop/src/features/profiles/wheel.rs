//! The General settings editor for application-specific wheel settings.

use gpui::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window,
    prelude::FluentBuilder as _,
};
use gpui_kit::component::{Selectable as _, button::ButtonVariants as _, h_flex, v_flex};
use openlogi_core::config::{ThumbwheelSensitivity, VerticalScrollSensitivity};

use super::{AppCatalogPicker, ProfileIconCache, scroll_profile_scope_bar};
use crate::state::{AppState, DeviceKey, DeviceRecord, StateEvent, StateEvents};
use crate::ui::commit_slider::{CommitSlider, SliderRange, SliderUnit};
use crate::ui::components::{Toggle, control_button, toggle_row};
use crate::ui::slider_row::sensitivity_row;
use crate::ui::theme::{self, Typography as _};

/// The catalog and active drags belong to this view. AppState owns the values.
pub(crate) struct WheelProfileEditor {
    icons: ProfileIconCache,
    catalog: Entity<AppCatalogPicker>,
    _catalog_obs: Subscription,
    _state_obs: Subscription,
}

impl WheelProfileEditor {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let icons = ProfileIconCache::default();
        let catalog = cx.new(|cx| AppCatalogPicker::new(icons.clone(), window, cx));
        let catalog_obs = cx.observe(&catalog, |_, _, cx| cx.notify());
        let state_obs = AppState::repaint_on(cx, |event| {
            matches!(
                event,
                StateEvent::SettingsChanged
                    | StateEvent::DeviceConfigChanged(_)
                    | StateEvent::ForegroundChanged
                    | StateEvent::LanguageChanged
            )
        });
        Self {
            icons,
            catalog,
            _catalog_obs: catalog_obs,
            _state_obs: state_obs,
        }
    }
}

impl Render for WheelProfileEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .min_w_0()
            .debug_selector(|| "main-wheel-profile-editor".into())
            .gap_3()
            .children(scroll_profile_scope_bar(&self.icons, &self.catalog, cx))
            .child(smooth_scrolling_row(cx))
            .child(Sensitivity::vertical().render(window, cx))
            .child(Sensitivity::horizontal().render(window, cx))
    }
}

/// Device selection matters while previewing an app's inherited thumb-wheel
/// value. Default always represents the global value, even with a mouse selected.
#[derive(Debug, PartialEq, Eq)]
struct EditingScope {
    app: Option<String>,
    device: Option<DeviceKey>,
}

impl EditingScope {
    fn current(cx: &App) -> Self {
        let state = AppState::try_read(cx);
        let app = state
            .and_then(AppState::editing_scroll_app)
            .map(str::to_owned);
        let device = app
            .as_ref()
            .and_then(|_| state.and_then(AppState::current_record))
            .map(DeviceRecord::device_key);
        Self { app, device }
    }

    fn apply(&self, cx: &mut App, edit: impl FnOnce(&mut AppState) -> StateEvents) {
        if *self == Self::current(cx) {
            AppState::apply(cx, edit);
        }
    }
}

fn smooth_scrolling_row(cx: &App) -> gpui::Div {
    let pal = theme::palette(cx);
    let state = AppState::try_read(cx);
    let app = state.filter(|state| state.editing_scroll_app().is_some());
    let scope = EditingScope::current(cx);
    let overridden = app.is_some_and(AppState::editing_smooth_scroll_overridden);
    let title = tr!("pointer.smooth_scrolling");
    v_flex()
        .gap_1()
        .child(toggle_row(
            "smooth-scroll-profile-setting",
            pal,
            title.clone(),
            if app.is_some() {
                inheritance_description(overridden)
            } else {
                tr!("pointer.smooth_scrolling_profiles_description")
            },
            Toggle::new("smooth-scroll-profile-toggle")
                .selected(state.is_some_and(AppState::editing_smooth_scroll))
                .on_change(move |enabled, _, cx| {
                    scope.apply(cx, |state| state.commit_editing_smooth_scroll(*enabled));
                }),
        ))
        .when(app.is_some(), |row| {
            row.child(inheritance_action(
                "smooth-scroll-profile-inheritance",
                &title,
                overridden,
                cx,
                move |state| {
                    if overridden {
                        state.reset_editing_smooth_scroll()
                    } else {
                        state.commit_editing_smooth_scroll(state.editing_smooth_scroll())
                    }
                },
            ))
        })
        .child(
            gpui::div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("pointer.smooth_wheel_description")),
        )
}

/// Both sensitivity sliders commit on release, pin the displayed value and can
/// independently return to their inherited value. Their units and fallback
/// rules remain distinct.
struct Sensitivity<T> {
    title: SharedString,
    description: SharedString,
    inheritance_id: &'static str,
    min: T,
    max: T,
    baseline: T,
    value: fn(&AppState) -> T,
    overridden: fn(&AppState) -> bool,
    commit: fn(&mut AppState, T) -> StateEvents,
    reset: fn(&mut AppState) -> StateEvents,
}

impl Sensitivity<VerticalScrollSensitivity> {
    fn vertical() -> Self {
        Self {
            title: tr!("pointer.vertical_scroll_sensitivity"),
            description: tr!("pointer.vertical_scroll_sensitivity_description"),
            inheritance_id: "vertical-scroll-profile-inheritance",
            min: VerticalScrollSensitivity::MIN,
            max: VerticalScrollSensitivity::MAX,
            baseline: VerticalScrollSensitivity::DEFAULT,
            value: AppState::editing_vertical_scroll_sensitivity,
            overridden: AppState::editing_vertical_scroll_sensitivity_overridden,
            commit: AppState::commit_editing_vertical_scroll_sensitivity,
            reset: AppState::reset_editing_vertical_scroll_sensitivity,
        }
    }
}

impl Sensitivity<ThumbwheelSensitivity> {
    fn horizontal() -> Self {
        Self {
            title: tr!("pointer.horizontal_scroll_sensitivity"),
            description: tr!("pointer.horizontal_scroll_sensitivity_description"),
            inheritance_id: "thumbwheel-profile-inheritance",
            min: ThumbwheelSensitivity::MIN,
            max: ThumbwheelSensitivity::MAX,
            baseline: ThumbwheelSensitivity::DEFAULT,
            value: AppState::editing_thumbwheel_sensitivity,
            overridden: AppState::editing_thumbwheel_sensitivity_overridden,
            commit: AppState::commit_editing_thumbwheel_sensitivity,
            reset: AppState::reset_editing_thumbwheel_sensitivity,
        }
    }
}

impl<T: SliderUnit + std::fmt::Display> Sensitivity<T> {
    fn render(self, window: &mut Window, cx: &mut App) -> gpui::Div {
        let state = AppState::try_read(cx);
        let value = state.map_or(self.baseline, self.value);
        let overridden = state
            .filter(|state| state.editing_scroll_app().is_some())
            .map(self.overridden);
        let scope = EditingScope::current(cx);
        let key = format!("{}:{scope:?}", self.inheritance_id);
        let slider = window.use_keyed_state(key, cx, |_, cx| {
            CommitSlider::new(
                SliderRange::new(self.min, self.max),
                value,
                cx,
                move |_, value, cx| scope.apply(cx, |state| (self.commit)(state, value)),
            )
        });
        let slider = slider.update(cx, |slider, cx| {
            slider.sync(value, window, cx);
            slider.slider().clone()
        });
        let pal = theme::palette(cx);
        v_flex()
            .gap_1()
            .child(sensitivity_row(
                self.title.clone(),
                self.description,
                &slider,
                self.max,
                (self.baseline, "1×".into()),
                cx,
            ))
            .when_some(overridden, |row, overridden| {
                row.child(
                    gpui::div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(inheritance_description(overridden)),
                )
                .child(inheritance_action(
                    self.inheritance_id,
                    &self.title,
                    overridden,
                    cx,
                    move |state| {
                        if overridden {
                            (self.reset)(state)
                        } else {
                            (self.commit)(state, (self.value)(state))
                        }
                    },
                ))
            })
    }
}

fn inheritance_description(overridden: bool) -> SharedString {
    if overridden {
        tr!("pointer.smooth_scrolling_override_description")
    } else {
        tr!("pointer.smooth_scrolling_inherited_description")
    }
}

fn inheritance_action(
    id: &'static str,
    setting: &str,
    overridden: bool,
    cx: &App,
    apply: impl Fn(&mut AppState) -> StateEvents + 'static,
) -> gpui::Div {
    let scope = EditingScope::current(cx);
    let label = if overridden {
        tr!("profiles.restore_inherited_setting")
    } else {
        tr!("profiles.use_current_value_for_app")
    };
    h_flex().child(
        control_button(id)
            .ghost()
            .accessibility_label(format!("{setting} — {label}"))
            .label(label)
            .debug_selector(move || id.into())
            .on_click(move |_, _, cx| scope.apply(cx, &apply)),
    )
}

#[cfg(test)]
mod tests {
    use std::{cell::OnceCell, rc::Rc};

    use gpui::TestAppContext;
    use gpui_kit::component::slider::SliderEvent;
    use openlogi_core::config::Config;

    use super::*;
    use crate::app::tests::click_control;
    use crate::services::assets::AssetResolver;
    use crate::state::Sources;

    #[gpui::test]
    fn a_late_slider_release_cannot_overwrite_the_new_profile(cx: &mut TestAppContext) {
        let old = Rc::new(OnceCell::new());
        let _observer = cx.update(|cx| {
            gpui_kit::init(cx);
            theme::register_builtin_themes(cx);
            let (commands, _) = tokio::sync::mpsc::unbounded_channel();
            let mut config = Config::ephemeral();
            let _ = config.set_app_vertical_scroll_sensitivity(
                "com.example.game",
                VerticalScrollSensitivity::try_new(22).unwrap(),
            );
            let resolver = AssetResolver::new();
            let sources = Sources::in_memory(config, &resolver, commands);
            AppState::set_global(cx.new(|_| AppState::new(sources)), cx);
            // Retain the original owner so its release subscription stays live
            // even after switching profiles unmounts the old control.
            let old = old.clone();
            cx.observe_new::<CommitSlider<VerticalScrollSensitivity>>(move |_, _, cx| {
                old.get_or_init(|| cx.entity());
            })
        });
        let (_, visual) = cx.add_window_view(WheelProfileEditor::new);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let old = old.get().unwrap();
        let slider = old.read_with(visual, |old, _| old.slider().clone());
        slider.update(visual, |_, cx| cx.emit(SliderEvent::Change(81.0.into())));

        click_control(visual, "scroll-profile:app:com.example.game");
        slider.update(visual, |_, cx| cx.emit(SliderEvent::Release(81.0.into())));
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(
                old.read(cx).seated().into_inner(),
                81,
                "the late release arrived"
            );
            let state = AppState::try_read(cx).unwrap();
            assert_eq!(state.editing_scroll_app(), Some("com.example.game"));
            assert_eq!(state.editing_vertical_scroll_sensitivity().into_inner(), 22);
            assert_eq!(
                state.app_settings().vertical_scroll_sensitivity,
                VerticalScrollSensitivity::DEFAULT
            );
        });
    }
}
