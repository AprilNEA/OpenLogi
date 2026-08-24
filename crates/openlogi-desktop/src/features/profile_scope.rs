//! Profile context bar for the Buttons workspace.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use gpui::{
    Anchor, AnyElement, App, Image, InteractiveElement, IntoElement, ParentElement, Role,
    StatefulInteractiveElement as _, Styled, Window, div, img, prelude::FluentBuilder as _, px,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    Icon, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariant, ButtonVariants as _},
    dialog::DialogButtonProps,
    h_flex,
    popover::Popover,
};

use crate::state::AppState;
use crate::ui::components::MenuRow;
use crate::ui::theme::{self, Palette, SelectableStyle as _, Typography as _};

use super::mouse::picker::{compact_panel, divider, title};

const PROFILE_CONTROL_H: f32 = 30.;

#[derive(Clone)]
struct ProfileChoice {
    app: String,
    name: String,
    icon: Option<Arc<Image>>,
    override_count: usize,
    persisted: bool,
}

/// Installed application icons are immutable for a GUI session. Keep AppKit
/// lookup and image encoding out of the render loop after the first sighting.
#[derive(Default)]
pub(crate) struct ProfileIconCache {
    icons: HashMap<String, Option<Arc<Image>>>,
}

impl ProfileIconCache {
    fn icon(&mut self, app: &str) -> Option<Arc<Image>> {
        self.icons
            .entry(app.to_string())
            .or_insert_with(|| crate::platform::app_icon::application_icon(app))
            .clone()
    }
}

/// A direct profile switcher. The foreground app may change which profile is
/// active, but never changes which profile this editor has open.
pub fn profile_scope_bar(
    pal: Palette,
    icons: &mut ProfileIconCache,
    cx: &App,
) -> Option<AnyElement> {
    let state = AppState::try_read(cx)?;
    if !state.current_device_is_persistent() {
        return None;
    }
    let editing_app = state.editing_app().map(str::to_string);
    let mut profiles: Vec<ProfileChoice> = state
        .app_profiles()
        .map(|(app, count)| ProfileChoice {
            app: app.to_string(),
            name: state
                .recent_app_name(app)
                .map_or_else(|| friendly_app_name(app), str::to_string),
            icon: icons.icon(app),
            override_count: count,
            persisted: true,
        })
        .collect();

    if let Some(app) = editing_app.as_deref()
        && !profiles.iter().any(|profile| profile.app == app)
    {
        profiles.push(ProfileChoice {
            app: app.to_string(),
            name: state
                .recent_app_name(app)
                .map_or_else(|| friendly_app_name(app), str::to_string),
            icon: icons.icon(app),
            override_count: 0,
            persisted: false,
        });
    }
    profiles.sort_by_key(|profile| profile.name.to_lowercase());
    let recent_apps: Vec<(String, String)> = state
        .recent_apps()
        .map(|(app, name)| (app.to_string(), name.to_string()))
        .collect();

    let persisted_ids: Vec<String> = profiles
        .iter()
        .filter(|profile| profile.persisted)
        .map(|profile| profile.app.clone())
        .collect();
    let available_apps: Vec<ProfileChoice> = recent_apps
        .into_iter()
        .filter(|(app, _)| {
            !persisted_ids.iter().any(|existing| existing == app)
                && editing_app.as_deref() != Some(app.as_str())
        })
        .map(|(app, name)| ProfileChoice {
            icon: icons.icon(&app),
            app,
            name,
            override_count: 0,
            persisted: false,
        })
        .collect();

    Some(profile_scope_content(
        editing_app.as_deref(),
        &profiles,
        available_apps,
        pal,
    ))
}

/// Profile inheritance and active-app context shown above the device canvas.
pub(crate) fn profile_canvas_status(pal: Palette, cx: &App) -> Option<AnyElement> {
    let state = AppState::try_read(cx)?;
    if !state.current_device_is_persistent() {
        return None;
    }
    let editing_app = state.editing_app().map(|app| {
        state
            .recent_app_name(app)
            .map_or_else(|| friendly_app_name(app), str::to_string)
    });
    let override_count = state.editing_app_overrides().map_or(0, BTreeMap::len);
    let summary = profile_summary(editing_app.as_deref(), override_count);
    let active = state
        .active_profile_name()
        .map_or_else(|| tr!("Default"), gpui::SharedString::from);

    Some(
        h_flex()
            .flex_none()
            .w_full()
            .items_start()
            .gap_3()
            .px_4()
            .pt_4()
            .text_caption()
            .text_color(pal.text_muted)
            .child(div().flex_1().min_w_0().child(summary))
            .child(
                div()
                    .flex_none()
                    .child(tr!("Active: %{profile}", profile => active)),
            )
            .into_any_element(),
    )
}

fn profile_scope_content(
    editing_app: Option<&str>,
    profiles: &[ProfileChoice],
    available_apps: Vec<ProfileChoice>,
    pal: Palette,
) -> AnyElement {
    let default_selected = editing_app.is_none();
    let selected_profile = editing_app
        .and_then(|app| profiles.iter().find(|profile| profile.app == app))
        .cloned();
    let profile_tabs = profiles
        .iter()
        .enumerate()
        .map(|(index, profile)| {
            let selected = editing_app == Some(profile.app.as_str());
            let app = profile.app.clone();
            profile_tab(
                ("app-profile", index),
                profile.name.clone(),
                Some(application_mark(profile.icon.clone(), &profile.name, pal)),
                selected,
                pal,
            )
            .on_click(move |_event, _window, cx| {
                AppState::update_bindings(cx, |state| {
                    state.set_editing_app(Some(app.clone()));
                });
            })
        })
        .collect::<Vec<_>>();

    h_flex()
        .flex_shrink_0()
        .w_full()
        .items_center()
        .gap_2()
        .border_b_1()
        .border_color(pal.border)
        .bg(pal.panel)
        .px_4()
        .py_2()
        .child(
            div()
                .flex_none()
                .text_body()
                .text_color(pal.text_muted)
                .child(tr!("Profile")),
        )
        .child(
            h_flex()
                .id("profile-tabs-scroll")
                .flex_1()
                .min_w_0()
                .items_center()
                .gap_1()
                .overflow_x_scroll()
                .child(
                    profile_tab(
                        "default-profile",
                        tr!("Default"),
                        None,
                        default_selected,
                        pal,
                    )
                    .on_click(|_event, _window, cx| {
                        AppState::update_bindings(cx, |state| {
                            state.set_editing_app(None);
                        });
                    }),
                )
                .children(profile_tabs),
        )
        .child(add_app_popover(available_apps, pal))
        .when_some(
            selected_profile.filter(|profile| profile.persisted),
            |row, profile| row.child(profile_options_popover(profile, pal)),
        )
        .into_any_element()
}

fn profile_tab(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    leading: Option<AnyElement>,
    selected: bool,
    pal: Palette,
) -> BaseButton {
    let label = label.into();
    BaseButton::new(id)
        .role(Role::Tab)
        .selected(selected)
        .accessibility_label(label.clone())
        .aria_selected(selected)
        .flex()
        .flex_none()
        .items_center()
        .gap_1p5()
        .h(px(PROFILE_CONTROL_H))
        .px_2p5()
        .rounded(pal.control_radius)
        .cursor_pointer()
        .text_body()
        .text_color(pal.text_primary)
        .selected_fill(selected)
        .hover(move |tab| {
            tab.bg(if selected {
                theme::accent_tint_hover()
            } else {
                pal.control_hover
            })
        })
        .focus_visible(move |tab| {
            tab.bg(if selected {
                theme::accent_tint_hover()
            } else {
                pal.control_hover
            })
        })
        .children(leading)
        .child(label)
}

fn application_mark(icon: Option<Arc<Image>>, name: &str, pal: Palette) -> AnyElement {
    if let Some(icon) = icon {
        return img(icon).size(px(18.)).flex_none().into_any_element();
    }

    let initial = name
        .chars()
        .find(|character| !character.is_whitespace())
        .map_or_else(|| "?".to_string(), |character| character.to_string());
    h_flex()
        .size(px(18.))
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .bg(pal.muted)
        .text_caption()
        .text_color(pal.text_muted)
        .child(initial)
        .into_any_element()
}

fn profile_summary(editing_app: Option<&str>, override_count: usize) -> gpui::SharedString {
    let Some(app) = editing_app else {
        return tr!("Default bindings apply unless an app profile overrides them.");
    };
    match override_count {
        0 => tr!(
            "No overrides yet. Select a button to customize for %{app}.",
            app => app
        ),
        1 => tr!(
            "%{app} overrides 1 button. Others inherit Default.",
            app => app
        ),
        count => tr!(
            "%{app} overrides %{count} buttons. Others inherit Default.",
            app => app,
            count => count
        ),
    }
}

fn add_app_popover(apps: Vec<ProfileChoice>, pal: Palette) -> AnyElement {
    Popover::new("add-app-popover")
        .anchor(Anchor::TopRight)
        .trigger(
            Button::new("add-app-profile")
                .outline()
                .small()
                .h(px(PROFILE_CONTROL_H))
                .icon(IconName::Plus)
                .label(tr!("Add app")),
        )
        .content(move |_state, _window, cx| {
            let popover = cx.entity().downgrade();
            let rows = apps
                .iter()
                .enumerate()
                .map(|(index, choice)| {
                    let app = choice.app.clone();
                    let popover = popover.clone();
                    MenuRow::new(("recent-app", index))
                        .role(Role::MenuItem)
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(application_mark(choice.icon.clone(), &choice.name, pal))
                                .child(choice.name.clone()),
                        )
                        .on_click(move |_event, window, cx| {
                            AppState::update_bindings(cx, |state| {
                                state.set_editing_app(Some(app.clone()));
                            });
                            if let Some(popover) = popover.upgrade() {
                                popover.update(cx, |state, cx| state.dismiss(window, cx));
                            }
                        })
                })
                .collect::<Vec<_>>();

            compact_panel(pal)
                .w(px(260.))
                .child(title(tr!("Add app profile"), pal))
                .child(divider(pal))
                .when(rows.is_empty(), |card| {
                    card.child(
                        div()
                            .px_2()
                            .py_2()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(tr!("Open an app to add it here.")),
                    )
                })
                .children(rows)
        })
        .into_any_element()
}

fn profile_options_popover(profile: ProfileChoice, pal: Palette) -> AnyElement {
    Popover::new("profile-options-popover")
        .anchor(Anchor::TopRight)
        .trigger(
            Button::new("profile-options")
                .ghost()
                .xsmall()
                .icon(IconName::Ellipsis),
        )
        .content(move |_state, _window, cx| {
            let popover = cx.entity().downgrade();
            let profile = profile.clone();
            compact_panel(pal)
                .w(px(224.))
                .child(title(tr!("Profile options"), pal))
                .child(divider(pal))
                .child(
                    MenuRow::new("remove-profile")
                        .role(Role::MenuItem)
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(Icon::new(IconName::Close).size_4())
                                .child(tr!("Remove profile…")),
                        )
                        .on_click(move |_event, window, cx| {
                            if let Some(popover) = popover.upgrade() {
                                popover.update(cx, |state, cx| state.dismiss(window, cx));
                            }
                            open_remove_confirmation(window, cx, &profile);
                        }),
                )
        })
        .into_any_element()
}

fn open_remove_confirmation(window: &mut Window, cx: &mut App, profile: &ProfileChoice) {
    let question = match profile.override_count {
        1 => tr!(
            "Remove %{app} profile and its 1 override?",
            app => profile.name.clone()
        ),
        count => tr!(
            "Remove %{app} profile and its %{count} overrides?",
            app => profile.name.clone(),
            count => count
        ),
    };
    window.open_alert_dialog(cx, move |alert, _, _| {
        alert
            .title(question.clone())
            .description(tr!(
                "This deletes the custom button bindings in this profile. Default bindings are not affected."
            ))
            .button_props(
                DialogButtonProps::default()
                    .ok_text(tr!("Remove profile"))
                    .ok_variant(ButtonVariant::Danger)
                    .cancel_text(tr!("Cancel"))
                    .show_cancel(true),
            )
            .on_ok(move |_event, _window, cx| {
                AppState::update_bindings(cx, |state| {
                    state.remove_editing_app_profile();
                });
                true
            })
    });
}

/// Derive a readable fallback from a profile identifier when the agent has not
/// reported that application in this session. The identifier remains the
/// matching key; only its last human-shaped component is presented.
pub(crate) fn friendly_app_name(identifier: &str) -> String {
    if let Some(path) = identifier.strip_prefix("exe:") {
        let name = path
            .rsplit(['/', '\\'])
            .find(|part| !part.is_empty())
            .unwrap_or(path);
        return name.trim_end_matches(".exe").to_string();
    }
    identifier
        .rsplit('.')
        .find(|part| !part.is_empty())
        .unwrap_or(identifier)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::friendly_app_name;

    #[test]
    fn profile_identifiers_have_a_readable_fallback() {
        assert_eq!(friendly_app_name("com.google.Chrome"), "Chrome");
        assert_eq!(friendly_app_name("exe:C:\\Tools\\Zed.exe"), "Zed");
    }
}
