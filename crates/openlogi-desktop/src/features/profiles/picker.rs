//! Add-application popover for per-application profiles.

use crate::ui::theme::FocusRingStyle as _;
use std::sync::Arc;

use gpui::{
    Anchor, Context, Entity, InteractiveElement, IntoElement, ParentElement, Role,
    StatefulInteractiveElement as _, Styled, UniformListScrollHandle, WeakEntity, div,
    prelude::FluentBuilder as _, px, uniform_list,
};
use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::{
    Icon, IconName, h_flex,
    popover::{Popover, PopoverState},
    scroll::ScrollableElement as _,
    v_flex,
};

use super::catalog::{AppCatalogPicker, ApplicationIconState, ProfileIconCache};
use super::shell::application_mark;
use super::{AddAppChoices, CatalogPresentation, ProfileChoice, ProfileScopeActions};
use crate::features::binding_editor::{compact_panel, divider, title};
use crate::ui::components::{MenuRow, control_button, control_input};
use crate::ui::theme::{Palette, PressableStyle as _, SelectableStyle as _, Typography as _};

const APP_ROW_H: f32 = 44.;
/// Icon tile edge inside picker rows: the height of the two-line text block,
/// so the 64 px source rendition maps 1:1 at 2× scale.
const ROW_ICON_EDGE: f32 = 32.;

pub(super) fn add_app_popover(
    id_base: &'static str,
    choices: AddAppChoices,
    catalog: Entity<AppCatalogPicker>,
    icons: ProfileIconCache,
    actions: ProfileScopeActions,
    pal: Palette,
) -> impl IntoElement {
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
                .debug_selector(move || format!("{id_base}:add-app-profile"))
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
            // The popover's positioner keeps an 8 px gutter but cannot resize
            // oversized content. Reserve that gutter and any client-side frame.
            let inset = window.client_inset().unwrap_or_default() + px(8.);
            let room = window.viewport_size().map(|edge| (edge - inset * 2.).max(px(0.)));
            add_app_content(id_base, &choices, &catalog, &icons, &actions, pal, cx)
                .max_w(room.width)
                .max_h(room.height)
        })
}

fn add_app_content(
    id_base: &'static str,
    choices: &AddAppChoices,
    catalog: &Entity<AppCatalogPicker>,
    icons: &ProfileIconCache,
    actions: &ProfileScopeActions,
    pal: Palette,
    cx: &mut Context<PopoverState>,
) -> gpui::Div {
    let popover = cx.entity().downgrade();
    let catalog_state = catalog.read(cx);
    let search = catalog_state.search();
    let query = search.read(cx).value().trim().to_lowercase();
    let show_applications = catalog_state.expanded() || !query.is_empty();
    let list_scroll = catalog_state.list_scroll();
    catalog.update(cx, |picker, cx| {
        for choice in &choices.recent {
            picker.ensure_icon(&choice.icon, cx);
        }
    });
    let recent_rows = choices
        .recent
        .iter()
        .filter(|choice| profile_matches_query(choice, &query))
        .cloned()
        .map(|choice| {
            let icon = icons.state(&choice.icon);
            application_row(id_base, choice, icon, actions.clone(), pal, popover.clone())
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
        .w(px(320.))
        .child(title(tr!("profiles.add_app_profile"), pal))
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
            .child(
                v_flex()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .children(recent_rows),
            )
        })
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
                actions.clone(),
                list_popover,
                &list_scroll,
                pal,
            ))
        })
        .when(show_applications && no_matches, |card| {
            card.child(catalog_message(tr!("profiles.no_applications_found"), pal))
        })
}

/// The scrollable catalog body: a `uniform_list` capped at six rows, with a
/// scrollbar signalling position in the full inventory. Rows resolve their
/// icons as they enter the viewport.
fn catalog_list(
    id_base: &'static str,
    rows: Arc<Vec<ProfileChoice>>,
    catalog: Entity<AppCatalogPicker>,
    actions: ProfileScopeActions,
    popover: WeakEntity<PopoverState>,
    scroll: &UniformListScrollHandle,
    pal: Palette,
) -> gpui::Div {
    let count = rows.len();
    div()
        .relative()
        .min_h_0()
        .h(px(application_list_height(count)))
        .w_full()
        .child(
            uniform_list(format!("{id_base}:application-catalog-list"), count, {
                move |visible_range, _window, cx| {
                    catalog.update(cx, |picker, cx| {
                        visible_range
                            .map(|index| {
                                let choice = rows[index].clone();
                                picker.ensure_icon(&choice.icon, cx);
                                let icon = picker.icon_state(&choice.icon);
                                application_row(
                                    id_base,
                                    choice,
                                    icon,
                                    actions.clone(),
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
        .justify_start()
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
        .pressable_fill(expanded, pal)
        .focus_visible(move |style| style.focus_ring(pal))
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
    icon: ApplicationIconState,
    actions: ProfileScopeActions,
    pal: Palette,
    popover: WeakEntity<PopoverState>,
) -> gpui::Div {
    let app = choice.app.clone();
    let selector = format!("{id_base}:catalog-app:{}", choice.app);
    let aria_label = format!("{}: {}", choice.name, choice.app);
    div().flex_none().h(px(APP_ROW_H)).child(
        MenuRow::new(format!("{id_base}:catalog-app:{}", choice.app))
            .role(Role::MenuItem)
            .debug_selector(move || selector.clone())
            .aria_label(aria_label)
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
                actions.select(Some(app.clone()), cx);
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
        AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _, Render,
        ScrollDelta, Styled as _, Subscription, TestAppContext, Window, point, px, size,
    };
    use gpui_kit::component::v_flex;
    use gpui_kit::test::TestWindowExt as _;

    use super::{
        AddAppChoices, AppCatalogPicker, CatalogPresentation, ProfileChoice, ProfileIconCache,
        ProfileScopeActions, add_app_popover,
    };
    use crate::ui::theme;

    struct PickerFixture {
        catalog: Entity<AppCatalogPicker>,
        icons: ProfileIconCache,
        selected: Rc<RefCell<Option<String>>>,
        _catalog_obs: Subscription,
    }

    impl Render for PickerFixture {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let rows = |prefix: &str, count| {
                (0..count)
                    .map(|index| {
                        let app = format!("{prefix}-{index:02}");
                        ProfileChoice::application(app.clone(), app)
                    })
                    .collect()
            };
            let selected = self.selected.clone();
            v_flex()
                .w_full()
                .pt(px(130.))
                .items_end()
                .child(add_app_popover(
                    "picker",
                    AddAppChoices {
                        recent: rows("recent", 12),
                        catalog: CatalogPresentation::Ready(rows("installed", 30)),
                    },
                    self.catalog.clone(),
                    self.icons.clone(),
                    ProfileScopeActions::wheel_profiles(
                        move |app, _| {
                            selected.replace(app);
                        },
                        |_, _, _| {},
                    ),
                    theme::palette(cx),
                ))
        }
    }

    #[gpui::test]
    fn recent_and_installed_apps_stay_reachable_in_a_short_picker(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        cx.update(gpui_kit::init);
        let selected: Rc<RefCell<Option<String>>> = Rc::default();
        let (_, visual) = cx.add_window_view({
            let selected = selected.clone();
            move |window, cx| {
                let icons = ProfileIconCache::default();
                let catalog = cx.new(|cx| AppCatalogPicker::new(icons.clone(), window, cx));
                let catalog_obs = cx.observe(&catalog, |_, _, cx| cx.notify());
                PickerFixture {
                    catalog,
                    icons,
                    selected,
                    _catalog_obs: catalog_obs,
                }
            }
        });
        let viewport = Bounds::new(point(px(0.), px(0.)), size(px(300.), px(400.)));
        visual.simulate_resize(viewport.size);
        visual.update(|window, cx| {
            window.render_frame(cx);
            window.click("picker:add-app-profile", cx);
            let toggle = window.find("picker:all-applications-toggle").bounds();
            assert!(
                viewport.contains(&toggle.origin) && viewport.contains(&toggle.bottom_right()),
                "all applications must remain reachable with a full recent list: {toggle:?}"
            );
            let to_end = ScrollDelta::Pixels(point(px(0.), px(-4000.)));
            window.scroll("picker:catalog-app:recent-00", to_end, cx);
            window.click("picker:catalog-app:recent-11", cx);
            assert_eq!(selected.borrow().as_deref(), Some("recent-11"));

            window.click("picker:add-app-profile", cx);
            window.click("picker:all-applications-toggle", cx);
            window.scroll("picker:catalog-app:installed-00", to_end, cx);
            window.click("picker:catalog-app:installed-29", cx);
            assert_eq!(selected.borrow().as_deref(), Some("installed-29"));

            window.click("picker:add-app-profile", cx);
            window.click("input-state", cx);
            window.input("installed-14", cx);
            assert!(window.try_find("picker:catalog-app:recent-00").is_none());
            assert!(window.find("picker:catalog-app:installed-14").visible());
            window.press("escape", cx);
            assert!(window.try_find("picker:all-applications-toggle").is_none());
        });
    }
}
