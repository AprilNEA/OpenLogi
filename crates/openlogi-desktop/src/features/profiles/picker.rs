//! Add-application popover for per-application profiles.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    Anchor, App, Context, Entity, InteractiveElement, IntoElement, ParentElement, Role,
    StatefulInteractiveElement as _, Styled, UniformListScrollHandle, WeakEntity, div,
    prelude::FluentBuilder as _, px, uniform_list,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    Icon, IconName, h_flex,
    popover::{Popover, PopoverState},
    scroll::ScrollableElement as _,
    v_flex,
};

use super::catalog::{AppCatalogPicker, AppIconState, ProfileIconCache};
use super::shell::application_mark;
use super::{AddAppChoices, CatalogPresentation, ProfileChoice, ProfileScopeActions};
use crate::features::mouse::picker::{compact_panel, divider, title};
use crate::state::AppState;
use crate::ui::components::{MenuRow, control_button, control_input};
use crate::ui::theme::{self, Palette, SelectableStyle as _, Typography as _};

const APP_ROW_H: f32 = 44.;
/// Icon tile edge inside picker rows: the height of the two-line text block,
/// so the 64 px source rendition maps 1:1 at 2× scale.
const ROW_ICON_EDGE: f32 = 32.;
type AppSelection = Rc<dyn Fn(ProfileChoice, &mut App)>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PickerMode {
    Profile,
    Application,
}

impl PickerMode {
    fn heading(self) -> gpui::SharedString {
        match self {
            Self::Profile => tr!("profiles.add_app_profile"),
            Self::Application => tr!("profiles.add_app"),
        }
    }
}

pub(super) fn add_app_popover(
    id_base: &'static str,
    choices: AddAppChoices,
    catalog: Entity<AppCatalogPicker>,
    icons: ProfileIconCache,
    actions: ProfileScopeActions,
    pal: Palette,
) -> impl IntoElement {
    let select: AppSelection = Rc::new(move |choice, cx| {
        actions.select(Some(choice.app), cx);
    });
    let catalog_on_open = catalog.clone();
    Popover::new(format!("{id_base}:add-app-popover"))
        .anchor(Anchor::TopRight)
        // `compact_panel` is the surface; the popover chrome would wrap it in
        // a second padded, differently-rounded box.
        .appearance(false)
        .trigger(
            control_button(format!("{id_base}:add-app-profile"))
                .outline()
                .icon(IconName::Plus)
                .label(tr!("profiles.add_app")),
        )
        .on_open_change(move |open, window, cx| {
            if *open {
                catalog_on_open.update(cx, |catalog, cx| catalog.clear_search(window, cx));
            }
        })
        .content(move |_state, window, cx| {
            let search = catalog.read(cx).search();
            crate::ui::components::localize_placeholder(
                &search,
                tr!("profiles.search_applications"),
                window,
                cx,
            );
            add_app_content(
                id_base,
                PickerMode::Profile,
                &choices,
                (&catalog, &icons),
                &select,
                pal,
                cx,
            )
        })
}

pub(crate) fn application_popover(
    id_base: &'static str,
    catalog: Entity<AppCatalogPicker>,
    icons: ProfileIconCache,
    on_select: impl Fn(String, String, &mut App) + 'static,
    pal: Palette,
) -> impl IntoElement {
    let select: AppSelection = Rc::new(move |choice, cx| {
        on_select(choice.launch_target, choice.name, cx);
    });
    let catalog_on_open = catalog.clone();
    Popover::new(format!("{id_base}:application-popover"))
        .anchor(Anchor::TopRight)
        .appearance(false)
        .trigger(
            control_button(format!("{id_base}:application-picker"))
                .outline()
                .icon(IconName::Folder)
                .label(tr!("profiles.add_app_dialog")),
        )
        .on_open_change(move |open, window, cx| {
            if *open {
                catalog_on_open.update(cx, |catalog, cx| catalog.clear_search(window, cx));
            }
        })
        .content(move |_state, window, cx| {
            let search = catalog.read(cx).search();
            crate::ui::components::localize_placeholder(
                &search,
                tr!("profiles.search_applications"),
                window,
                cx,
            );
            let recent_apps = AppState::try_read(cx)
                .map(|state| {
                    state
                        .recent_apps()
                        .map(|(app, name)| (app.to_string(), name.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let observed = recent_apps
                .iter()
                .map(|(app, _)| app.clone())
                .collect::<HashSet<_>>();
            let choices = application_choices(
                catalog
                    .read(cx)
                    .available_profiles(&observed, &HashSet::new()),
                &recent_apps,
            );
            add_app_content(
                id_base,
                PickerMode::Application,
                &choices,
                (&catalog, &icons),
                &select,
                pal,
                cx,
            )
        })
}

fn add_app_content(
    id_base: &'static str,
    mode: PickerMode,
    choices: &AddAppChoices,
    resources: (&Entity<AppCatalogPicker>, &ProfileIconCache),
    select: &AppSelection,
    pal: Palette,
    cx: &mut Context<PopoverState>,
) -> gpui::Div {
    let (catalog, icons) = resources;
    let popover = cx.entity().downgrade();
    let catalog_state = catalog.read(cx);
    let search = catalog_state.search();
    let query = search.read(cx).value().trim().to_lowercase();
    let show_applications = catalog_state.expanded() || !query.is_empty();
    let list_scroll = catalog_state.list_scroll();
    catalog.update(cx, |picker, cx| {
        for choice in &choices.recent {
            picker.ensure_icon(&choice.app, cx);
        }
    });
    let recent_rows = choices
        .recent
        .iter()
        .filter(|choice| profile_matches_query(choice, &query))
        .cloned()
        .map(|choice| {
            let icon = icons.state(&choice.app);
            application_row(id_base, choice, icon, select.clone(), pal, popover.clone())
        })
        .collect::<Vec<_>>();
    let application_rows = match &choices.catalog {
        CatalogPresentation::Ready(applications) => applications
            .iter()
            .filter(|choice| profile_matches_query(choice, &query))
            .cloned()
            .collect::<Vec<_>>(),
        CatalogPresentation::Loading | CatalogPresentation::Failed => Vec::new(),
    };
    let no_matches = matches!(&choices.catalog, CatalogPresentation::Ready(_))
        && application_rows.is_empty()
        && (query.is_empty() || recent_rows.is_empty());
    let catalog_for_toggle = catalog.clone();
    let list_catalog = catalog.clone();
    let list_popover = popover.clone();
    let list_len = application_rows.len();
    let application_rows = Arc::new(application_rows);

    compact_panel(pal)
        .w(px(match mode {
            PickerMode::Profile => 320.,
            PickerMode::Application => 280.,
        }))
        .child(title(mode.heading(), pal))
        .child(divider(pal))
        .child(
            control_input(&search)
                .cleanable(true)
                .prefix(IconName::Search),
        )
        .when(!recent_rows.is_empty(), |card| {
            card.child(
                div()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("profiles.recent_applications")),
            )
        })
        .children(recent_rows)
        .child(div().pt_1().w_full().child(divider(pal)))
        .child(applications_toggle(
            id_base,
            show_applications,
            catalog_for_toggle,
            pal,
        ))
        .when(
            show_applications && matches!(&choices.catalog, CatalogPresentation::Loading),
            |card| card.child(catalog_message(tr!("profiles.loading_applications"), pal)),
        )
        .when(
            show_applications && matches!(&choices.catalog, CatalogPresentation::Failed),
            |card| {
                card.child(catalog_message(
                    tr!("profiles.application_catalog_unavailable"),
                    pal,
                ))
            },
        )
        .when(show_applications && list_len > 0, |card| {
            card.child(catalog_list(
                id_base,
                application_rows,
                list_catalog,
                select.clone(),
                list_popover,
                &list_scroll,
                pal,
            ))
        })
        .when(show_applications && no_matches, |card| {
            card.child(catalog_message(tr!("profiles.no_applications_found"), pal))
        })
}

fn application_choices(
    catalog: CatalogPresentation,
    recent_apps: &[(String, String)],
) -> AddAppChoices {
    let recent_by_id = recent_apps
        .iter()
        .enumerate()
        .map(|(index, (app, name))| (app.as_str(), (index, name.as_str())))
        .collect::<HashMap<_, _>>();
    let CatalogPresentation::Ready(applications) = catalog else {
        return AddAppChoices {
            recent: Vec::new(),
            catalog,
        };
    };
    let (mut recent, remaining): (Vec<_>, Vec<_>) = applications
        .into_iter()
        .partition(|choice| recent_by_id.contains_key(choice.app.as_str()));
    for choice in &mut recent {
        choice.name = recent_by_id[choice.app.as_str()].1.to_string();
    }
    recent.sort_by_key(|choice| recent_by_id[choice.app.as_str()].0);
    AddAppChoices {
        recent,
        catalog: CatalogPresentation::Ready(remaining),
    }
}

/// The scrollable catalog body: a `uniform_list` capped at six rows, with a
/// scrollbar signalling position in the full inventory. Rows resolve their
/// icons as they enter the viewport.
fn catalog_list(
    id_base: &'static str,
    rows: Arc<Vec<ProfileChoice>>,
    catalog: Entity<AppCatalogPicker>,
    select: AppSelection,
    popover: WeakEntity<PopoverState>,
    scroll: &UniformListScrollHandle,
    pal: Palette,
) -> gpui::Div {
    let count = rows.len();
    div()
        .h(px(application_list_height(count)))
        .w_full()
        .child(
            uniform_list(format!("{id_base}:application-catalog-list"), count, {
                move |visible_range, _window, cx| {
                    catalog.update(cx, |picker, cx| {
                        visible_range
                            .map(|index| {
                                let choice = rows[index].clone();
                                picker.ensure_icon(&choice.app, cx);
                                let icon = picker.icon_state(&choice.app);
                                application_row(
                                    id_base,
                                    choice,
                                    icon,
                                    select.clone(),
                                    pal,
                                    popover.clone(),
                                )
                            })
                            .collect::<Vec<_>>()
                    })
                }
            })
            .track_scroll(scroll)
            .h_full()
            .w_full(),
        )
        .vertical_scrollbar(scroll)
}

fn applications_toggle(
    id_base: &'static str,
    expanded: bool,
    catalog: Entity<AppCatalogPicker>,
    pal: Palette,
) -> impl IntoElement {
    BaseButton::new(format!("{id_base}:all-applications-toggle"))
        .role(Role::Button)
        .aria_expanded(expanded)
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1p5()
        .rounded(pal.control_radius)
        .text_body()
        // Muted while collapsed so the section control reads apart from the
        // application rows; primary once open, over the accent fill.
        .text_color(if expanded {
            pal.text_primary
        } else {
            pal.text_muted
        })
        .selected_fill(expanded)
        .hover(move |button| {
            button.bg(if expanded {
                theme::accent_tint_hover()
            } else {
                pal.control_hover
            })
        })
        .focus_visible(move |button| {
            button.bg(if expanded {
                theme::accent_tint_hover()
            } else {
                pal.control_hover
            })
        })
        .child(
            // The chevron sits centred in a row-icon-wide slot so the label
            // starts where the application names below do.
            h_flex()
                .w(px(ROW_ICON_EDGE))
                .flex_none()
                .justify_center()
                .child(
                    Icon::new(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size_4(),
                ),
        )
        .child(tr!("profiles.all_applications"))
        .on_click(move |_event, _window, cx| {
            catalog.update(cx, AppCatalogPicker::toggle_expanded);
        })
}

fn profile_matches_query(choice: &ProfileChoice, query: &str) -> bool {
    query.is_empty()
        || choice.name.to_lowercase().contains(query)
        || choice.app.to_lowercase().contains(query)
}

fn application_row(
    id_base: &'static str,
    choice: ProfileChoice,
    icon: AppIconState,
    select: AppSelection,
    pal: Palette,
    popover: WeakEntity<PopoverState>,
) -> gpui::Div {
    let selected = choice.clone();
    div().h(px(APP_ROW_H)).child(
        MenuRow::new(format!("{id_base}:catalog-app:{}", choice.app))
            .role(Role::MenuItem)
            .child(
                h_flex()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .child(application_mark(icon, &choice.name, ROW_ICON_EDGE, pal))
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().truncate().text_body().child(choice.name))
                            .child(
                                div()
                                    .truncate()
                                    .text_caption()
                                    .text_color(pal.text_muted)
                                    .child(choice.app),
                            ),
                    ),
            )
            .on_click(move |_event, window, cx| {
                select(selected.clone(), cx);
                if let Some(popover) = popover.upgrade() {
                    popover.update(cx, |state, cx| state.dismiss(window, cx));
                }
            }),
    )
}

fn catalog_message(message: gpui::SharedString, pal: Palette) -> impl IntoElement {
    div()
        .px_2()
        .py_2()
        .text_caption()
        .text_color(pal.text_muted)
        .child(message)
}

fn application_list_height(rows: usize) -> f32 {
    match rows.min(6) {
        0 => 0.,
        1 => APP_ROW_H,
        2 => APP_ROW_H * 2.,
        3 => APP_ROW_H * 3.,
        4 => APP_ROW_H * 4.,
        5 => APP_ROW_H * 5.,
        _ => APP_ROW_H * 6.,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{
        Context, InteractiveElement as _, IntoElement, Modifiers, ParentElement as _, Render,
        ScrollDelta, ScrollWheelEvent, Styled as _, TestAppContext, TouchPhase, Window, div, point,
        px, uniform_list,
    };
    use gpui_component::button::Button;
    use gpui_component::popover::Popover;

    use super::{APP_ROW_H, application_choices};
    use crate::features::mouse::picker::compact_panel;
    use crate::features::profiles::{CatalogPresentation, ProfileChoice};
    use crate::ui::components::MenuRow;
    use crate::ui::theme;

    /// The Add-app popover structure — unstyled popover, `compact_panel`
    /// surface, `uniform_list` catalog — with rows that record activation
    /// instead of touching `AppState`.
    struct PickerScrollHarness {
        clicked: Rc<RefCell<Option<usize>>>,
    }

    impl Render for PickerScrollHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let pal = theme::palette(cx);
            let clicked = self.clicked.clone();
            Popover::new("add-app-popover")
                .appearance(false)
                .trigger(Button::new("add-app-profile").label("Add app"))
                .content(move |_state, _window, _cx| {
                    let clicked = clicked.clone();
                    compact_panel(pal).w(px(320.)).child(
                        uniform_list(
                            "application-catalog-list",
                            30,
                            move |range, _window, _cx| {
                                range
                                    .map(|index| {
                                        let clicked = clicked.clone();
                                        div()
                                            .h(px(APP_ROW_H))
                                            .debug_selector(move || format!("app-row-{index}"))
                                            .child(
                                                MenuRow::new(("catalog-app", index))
                                                    .child(format!("App {index}"))
                                                    .on_click(move |_, _, _| {
                                                        clicked.replace(Some(index));
                                                    }),
                                            )
                                    })
                                    .collect::<Vec<_>>()
                            },
                        )
                        .h(px(APP_ROW_H * 6.))
                        .w_full(),
                    )
                })
        }
    }

    #[gpui::test]
    fn catalog_list_scrolls_inside_the_picker_popover(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let clicked: Rc<RefCell<Option<usize>>> = Rc::default();
        let (_, cx) = cx.add_window_view({
            let clicked = clicked.clone();
            move |_, _| PickerScrollHarness { clicked }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        // Open through the trigger, then let the deferred popup capture its
        // anchor and paint the content on the following frame.
        cx.simulate_click(point(px(20.), px(10.)), Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let first_row = cx
            .debug_bounds("app-row-0")
            .expect("the catalog list renders in the popover");
        let cursor = first_row.center();
        cx.simulate_click(cursor, Modifiers::default());
        assert_eq!(*clicked.borrow(), Some(0));

        cx.simulate_event(ScrollWheelEvent {
            position: cursor,
            delta: ScrollDelta::Lines(point(0., -5.)),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        assert_ne!(
            cx.debug_bounds("app-row-0"),
            Some(first_row),
            "wheel over the catalog list must scroll it"
        );
        cx.simulate_click(cursor, Modifiers::default());
        assert_ne!(
            *clicked.borrow(),
            Some(0),
            "after scrolling a different row sits under the cursor"
        );
    }

    #[test]
    fn application_choices_separate_recent_apps_in_recency_order() {
        let choice = |app: &str, name: &str| ProfileChoice {
            app: app.to_string(),
            launch_target: format!("/Applications/{name}.app"),
            name: name.to_string(),
            override_count: 0,
            persisted: false,
        };
        let choices = application_choices(
            CatalogPresentation::Ready(vec![
                choice("app.alpha", "Alpha"),
                choice("app.beta", "Beta"),
                choice("app.gamma", "Gamma"),
            ]),
            &[
                ("app.beta".into(), "Beta Recent".into()),
                ("app.alpha".into(), "Alpha Recent".into()),
            ],
        );

        assert_eq!(
            choices
                .recent
                .iter()
                .map(|choice| choice.name.as_str())
                .collect::<Vec<_>>(),
            ["Beta Recent", "Alpha Recent"]
        );
        let CatalogPresentation::Ready(remaining) = choices.catalog else {
            panic!("catalog should remain ready");
        };
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].app, "app.gamma");
    }
}
