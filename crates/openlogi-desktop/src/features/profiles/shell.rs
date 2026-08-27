//! Reusable profile tabs and feature-independent controls.

use gpui::{
    App, Entity, InteractiveElement, IntoElement, ParentElement, RenderOnce, Role,
    StatefulInteractiveElement as _, Styled, Window, div, img, prelude::FluentBuilder as _, px,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    popover::Popover,
    spinner::Spinner,
};

use super::catalog::{AppCatalogPicker, AppIconState, ProfileIconCache};
use super::picker::add_app_popover;
use super::{ProfileChoice, ProfileScopeActions, ProfileScopeModel};
use crate::features::mouse::picker::{compact_panel, divider, title};
use crate::ui::components::MenuRow;
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
                .child(tr!("Profile")),
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
                        tr!("Default"),
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
        .when_some(
            selected_profile.filter(|profile| profile.persisted),
            |row, profile| {
                row.child(profile_options_popover(
                    shell.id_base,
                    profile,
                    shell.actions,
                    pal,
                ))
            },
        )
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
    icon: AppIconState,
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
        AppIconState::Ready(icon) => slot.child(img(icon).size(px(edge)).flex_none()),
        AppIconState::Loading => slot.child(
            Spinner::new()
                .with_size(px(edge * 0.6))
                .color(pal.text_muted),
        ),
        AppIconState::Missing => {
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

fn profile_options_popover(
    id_base: &'static str,
    profile: ProfileChoice,
    actions: ProfileScopeActions,
    pal: Palette,
) -> impl IntoElement {
    Popover::new(format!("{id_base}:profile-options-popover"))
        .anchor(gpui::Anchor::TopRight)
        // `compact_panel` is the surface; the popover chrome would wrap it in
        // a second padded, differently-rounded box.
        .appearance(false)
        .trigger(
            Button::new(format!("{id_base}:profile-options"))
                .ghost()
                .xsmall()
                .icon(IconName::Ellipsis),
        )
        .content(move |_state, _window, cx| {
            let popover = cx.entity().downgrade();
            let profile = profile.clone();
            let actions = actions.clone();
            compact_panel(pal)
                .w(px(224.))
                .child(title(tr!("Profile options"), pal))
                .child(divider(pal))
                .child(
                    MenuRow::new(format!("{id_base}:remove-profile"))
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
                            actions.remove(profile.clone(), window, cx);
                        }),
                )
        })
}
