//! Reusable profile tabs and feature-independent controls.

use gpui::{
    App, Entity, InteractiveElement, IntoElement, ParentElement, RenderOnce, Role,
    StatefulInteractiveElement as _, Styled, Window, div, img, prelude::FluentBuilder as _, px,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    IconName, Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    spinner::Spinner,
};

use super::catalog::{AppCatalogPicker, ApplicationIconState, ProfileIconCache};
use super::picker::add_app_popover;
use super::{ProfileChoice, ProfileScopeActions, ProfileScopeModel};
use crate::ui::components::control_button;
use crate::ui::theme::{self, Palette, SelectableStyle as _, Typography as _};

/// Icon edge inside single-line profile tabs.
const TAB_ICON_EDGE: f32 = 18.;

/// Feature-independent profile switcher. Feature adapters provide the
/// profiles and own selection/removal behavior through [`ProfileScopeActions`].
#[derive(IntoElement)]
pub(crate) struct ProfileScopeShell {
    id_base: &'static str,
    model: ProfileScopeModel,
    catalog: Entity<AppCatalogPicker>,
    icons: ProfileIconCache,
    actions: ProfileScopeActions,
}

impl ProfileScopeShell {
    pub(super) fn new(
        id_base: &'static str,
        model: ProfileScopeModel,
        catalog: Entity<AppCatalogPicker>,
        icons: ProfileIconCache,
        actions: ProfileScopeActions,
    ) -> Self {
        Self {
            id_base,
            model,
            catalog,
            icons,
            actions,
        }
    }
}

impl RenderOnce for ProfileScopeShell {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        profile_scope_content(self, pal)
    }
}

fn profile_scope_content(shell: ProfileScopeShell, pal: Palette) -> impl IntoElement + use<> {
    let default_selected = shell.model.editing_app.is_none();
    let selected_profile = shell
        .model
        .editing_app
        .as_deref()
        .and_then(|app| {
            shell
                .model
                .profiles
                .iter()
                .find(|profile| profile.app == app)
        })
        .cloned();
    let profile_tabs = shell
        .model
        .profiles
        .iter()
        .map(|profile| {
            let selected = shell.model.editing_app.as_deref() == Some(profile.app.as_str());
            let app = profile.app.clone();
            let actions = shell.actions.clone();
            profile_tab(
                format!("{}:app:{}", shell.id_base, profile.app),
                profile.name.clone(),
                Some(application_mark(
                    shell.icons.state(&profile.app),
                    &profile.name,
                    TAB_ICON_EDGE,
                    pal,
                )),
                selected,
                pal,
            )
            .on_click(move |_event, _window, cx| {
                actions.select(Some(app.clone()), cx);
            })
        })
        .collect::<Vec<_>>();
    let default_actions = shell.actions.clone();

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
                .child(tr!("profiles.profile")),
        )
        .child(
            h_flex()
                .id(format!("{}:tabs-scroll", shell.id_base))
                .flex_1()
                .min_w_0()
                .items_center()
                .gap_1()
                .overflow_x_scroll()
                .child(
                    profile_tab(
                        format!("{}:default", shell.id_base),
                        tr!("common.default"),
                        None,
                        default_selected,
                        pal,
                    )
                    .on_click(move |_event, _window, cx| {
                        default_actions.select(None, cx);
                    }),
                )
                .children(profile_tabs),
        )
        .child(add_app_popover(
            shell.id_base,
            shell.model.choices,
            shell.catalog,
            shell.icons,
            shell.actions.clone(),
            pal,
        ))
        .when_some(selected_profile, |row, profile| {
            row.child(profile_options_menu(shell.id_base, profile, shell.actions))
        })
}

fn profile_tab(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    leading: Option<gpui::Div>,
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
        .h(px(theme::CONTROL_H))
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

pub(super) fn application_mark(
    icon: ApplicationIconState,
    name: &str,
    edge: f32,
    pal: Palette,
) -> gpui::Div {
    let slot = h_flex()
        .size(px(edge))
        .flex_none()
        .items_center()
        .justify_center();
    match icon {
        ApplicationIconState::Ready(icon) => slot.child(img(icon).size(px(edge)).flex_none()),
        ApplicationIconState::Loading => slot.child(
            Spinner::new()
                .with_size(px(edge * 0.6))
                .color(pal.text_muted),
        ),
        ApplicationIconState::Missing => {
            let initial = name
                .chars()
                .find(|character| !character.is_whitespace())
                .map_or_else(|| "?".to_string(), |character| character.to_string());
            slot.rounded(px(edge / 4.))
                .bg(pal.muted)
                .map(|tile| {
                    if edge < 24. {
                        tile.text_caption()
                    } else {
                        tile.text_body()
                    }
                })
                .text_color(pal.text_muted)
                .child(initial)
        }
    }
}

fn profile_options_menu(
    id_base: &'static str,
    profile: ProfileChoice,
    actions: ProfileScopeActions,
) -> impl IntoElement {
    control_button(format!("{id_base}:profile-options"))
        .ghost()
        .label(tr!("profiles.profile_options"))
        .icon(IconName::ChevronDown)
        .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
            let reset_profile = profile.clone();
            let reset_actions = actions.clone();
            let all_profile = profile.clone();
            let all_actions = actions.clone();
            let profile = profile.clone();
            let actions = actions.clone();
            menu.label(profile.name.clone())
                .item(
                    PopupMenuItem::new(tr!("profiles.reset_profile_dialog"))
                        .disabled(!profile.persisted)
                        .on_click(move |_, window, cx| {
                            let profile = reset_profile.clone();
                            let actions = reset_actions.clone();
                            // Open after the menu restores focus to its trigger.
                            window.defer(cx, move |window, cx| actions.reset(profile, window, cx));
                        }),
                )
                .separator()
                .item(
                    PopupMenuItem::new(if profile.persisted {
                        tr!("profiles.remove_profile_dialog")
                    } else {
                        tr!("profiles.remove_profile")
                    })
                    .on_click(move |_event, window, cx| {
                        let profile = profile.clone();
                        let actions = actions.clone();
                        window.defer(cx, move |window, cx| actions.remove(profile, window, cx));
                    }),
                )
                .separator()
                .item(
                    PopupMenuItem::new(tr!("profiles.remove_all_profiles_dialog")).on_click(
                        move |_, window, cx| {
                            let profile = all_profile.clone();
                            let actions = all_actions.clone();
                            window.defer(cx, move |window, cx| {
                                actions.remove_all(profile, window, cx);
                            });
                        },
                    ),
                )
        })
}
