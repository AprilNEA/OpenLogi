//! Profile context bar for the Buttons workspace.

use gpui::{
    Anchor, AnyElement, App, BorrowAppContext as _, InteractiveElement, IntoElement, ParentElement,
    Role, StatefulInteractiveElement as _, Styled, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Icon, IconName, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariant, ButtonVariants as _},
    dialog::DialogButtonProps,
    h_flex,
    popover::Popover,
    v_flex,
};

use crate::state::AppState;
use crate::ui::components::MenuRow;
use crate::ui::theme::{self, Palette, SelectableStyle as _, Typography as _};

use super::mouse::picker::{divider, menu_card, title};

#[derive(Clone)]
struct ProfileChoice {
    app: String,
    name: String,
    override_count: usize,
    persisted: bool,
}

/// A direct profile switcher. The foreground app may change which profile is
/// active, but never changes which profile this editor has open.
pub fn profile_scope_bar(pal: Palette, cx: &App) -> Option<AnyElement> {
    let state = cx.try_global::<AppState>()?;
    if !state.current_device_is_persistent() {
        return None;
    }
    let editing_app = state.editing_app().map(str::to_string);
    let active_profile = state
        .active_profile_name()
        .map_or_else(|| tr!("Default"), gpui::SharedString::from);
    let mut profiles: Vec<ProfileChoice> = state
        .app_profiles()
        .map(|(app, count)| ProfileChoice {
            app: app.to_string(),
            name: state
                .recent_app_name(app)
                .map_or_else(|| friendly_app_name(app), str::to_string),
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
            override_count: 0,
            persisted: false,
        });
    }
    profiles.sort_by_key(|profile| profile.name.to_lowercase());
    let recent_apps: Vec<(String, String)> = state
        .recent_apps()
        .map(|(app, name)| (app.to_string(), name.to_string()))
        .collect();

    let summary = profile_summary(editing_app.as_deref(), &profiles);
    let persisted_ids: Vec<String> = profiles
        .iter()
        .filter(|profile| profile.persisted)
        .map(|profile| profile.app.clone())
        .collect();
    let available_apps: Vec<(String, String)> = recent_apps
        .into_iter()
        .filter(|(app, _)| {
            !persisted_ids.iter().any(|existing| existing == app)
                && editing_app.as_deref() != Some(app.as_str())
        })
        .collect();

    Some(profile_scope_content(
        editing_app.as_deref(),
        &active_profile,
        &profiles,
        available_apps,
        summary,
        pal,
    ))
}

fn profile_scope_content(
    editing_app: Option<&str>,
    active_profile: &gpui::SharedString,
    profiles: &[ProfileChoice],
    available_apps: Vec<(String, String)>,
    summary: gpui::SharedString,
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
            profile_tab(("app-profile", index), profile.name.clone(), selected, pal).on_click(
                move |_event, _window, cx| {
                    cx.update_global::<AppState, _>(|state, _| {
                        state.set_editing_app(Some(app.clone()));
                    });
                    cx.refresh_windows();
                },
            )
        })
        .collect::<Vec<_>>();

    v_flex()
        .flex_shrink_0()
        .w_full()
        .gap_1p5()
        .border_b_1()
        .border_color(pal.border)
        .bg(pal.surface)
        .px_4()
        .py_2()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .text_caption()
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
                            profile_tab("default-profile", tr!("Default"), default_selected, pal)
                                .on_click(|_event, _window, cx| {
                                    cx.update_global::<AppState, _>(|state, _| {
                                        state.set_editing_app(None);
                                    });
                                    cx.refresh_windows();
                                }),
                        )
                        .children(profile_tabs),
                )
                .child(add_app_popover(available_apps, pal))
                .when_some(
                    selected_profile.filter(|profile| profile.persisted),
                    |row, profile| row.child(profile_options_popover(profile, pal)),
                ),
        )
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap_4()
                .text_caption()
                .text_color(pal.text_muted)
                .child(summary)
                .child(
                    div()
                        .flex_none()
                        .child(tr!("Active: %{profile}", profile => active_profile.clone())),
                ),
        )
        .into_any_element()
}

fn profile_tab(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    selected: bool,
    pal: Palette,
) -> gpui::Stateful<gpui::Div> {
    h_flex()
        .id(id)
        .role(Role::Tab)
        .aria_selected(selected)
        .flex_none()
        .items_center()
        .px_2p5()
        .py_1()
        .rounded(pal.control_radius)
        .cursor_pointer()
        .text_body()
        .text_color(pal.text_primary)
        .selected_fill(selected)
        .hover(move |tab| {
            tab.bg(if selected {
                theme::accent_tint_hover()
            } else {
                pal.surface_hover
            })
        })
        .child(label.into())
}

fn profile_summary(editing_app: Option<&str>, profiles: &[ProfileChoice]) -> gpui::SharedString {
    let Some(app) = editing_app else {
        return tr!("Default bindings apply unless an app profile overrides them.");
    };
    let Some(profile) = profiles.iter().find(|profile| profile.app == app) else {
        return gpui::SharedString::default();
    };
    match profile.override_count {
        0 => tr!(
            "No overrides yet. Select a button to customize for %{app}.",
            app => profile.name.clone()
        ),
        1 => tr!(
            "%{app} overrides 1 button. Others inherit Default.",
            app => profile.name.clone()
        ),
        count => tr!(
            "%{app} overrides %{count} buttons. Others inherit Default.",
            app => profile.name.clone(),
            count => count
        ),
    }
}

fn add_app_popover(apps: Vec<(String, String)>, pal: Palette) -> AnyElement {
    Popover::new("add-app-popover")
        .anchor(Anchor::TopRight)
        .trigger(
            Button::new("add-app-profile")
                .outline()
                .xsmall()
                .icon(IconName::Plus)
                .label(tr!("Add app")),
        )
        .content(move |_state, _window, cx| {
            let popover = cx.entity().downgrade();
            let rows = apps
                .iter()
                .enumerate()
                .map(|(index, (app, name))| {
                    let app = app.clone();
                    let popover = popover.clone();
                    MenuRow::new(("recent-app", index))
                        .child(name.clone())
                        .on_click(move |_event, window, cx| {
                            cx.update_global::<AppState, _>(|state, _| {
                                state.set_editing_app(Some(app.clone()));
                            });
                            cx.refresh_windows();
                            if let Some(popover) = popover.upgrade() {
                                popover.update(cx, |state, cx| state.dismiss(window, cx));
                            }
                        })
                })
                .collect::<Vec<_>>();

            menu_card(pal)
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
            menu_card(pal)
                .w(px(224.))
                .child(title(tr!("Profile options"), pal))
                .child(divider(pal))
                .child(
                    MenuRow::new("remove-profile")
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
                cx.update_global::<AppState, _>(|state, _| {
                    state.remove_editing_app_profile();
                });
                cx.refresh_windows();
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
