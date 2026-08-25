//! Profile context bar for the Buttons workspace.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use appcatalog::{Application, ApplicationIdentity, IdentityKind};
use gpui::{
    Anchor, App, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    RenderImage, RenderOnce, Role, StatefulInteractiveElement as _, Styled, Subscription, Task,
    UniformListScrollHandle, WeakEntity, Window, div, img, prelude::FluentBuilder as _, px,
    uniform_list,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    Icon, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariant, ButtonVariants as _},
    dialog::DialogButtonProps,
    h_flex,
    input::{InputEvent, InputState},
    popover::{Popover, PopoverState},
    scroll::ScrollableElement as _,
    spinner::Spinner,
    v_flex,
};

use crate::state::AppState;
use crate::ui::components::{MenuRow, control_button, control_input};
use crate::ui::theme::{self, Palette, SelectableStyle as _, Typography as _};

use super::mouse::picker::{compact_panel, divider, title};

const APP_ROW_H: f32 = 44.;
/// Icon tile edge inside picker rows: the height of the two-line text block,
/// so the 64 px source rendition maps 1:1 at 2× scale.
const ROW_ICON_EDGE: f32 = 32.;
/// Icon edge inside single-line profile tabs.
const TAB_ICON_EDGE: f32 = 18.;

#[derive(Clone)]
struct ProfileChoice {
    app: String,
    name: String,
    override_count: usize,
    persisted: bool,
}

struct AddAppChoices {
    recent: Vec<ProfileChoice>,
    applications: Vec<ProfileChoice>,
    loading: bool,
    failed: bool,
}

/// Installed application icons are immutable for a GUI session. The store
/// only ever holds finished lookups — [`AppCatalogPicker::ensure_icon`] fills
/// it from the background executor, so a miss renders the monogram fallback
/// and the row repaints when the icon lands. AppKit never runs on the render
/// path.
#[derive(Clone, Default)]
pub(crate) struct ProfileIconCache {
    icons: Rc<RefCell<HashMap<String, Option<Arc<RenderImage>>>>>,
}

impl ProfileIconCache {
    fn state(&self, app: &str) -> AppIconState {
        match self.icons.borrow().get(app) {
            Some(Some(icon)) => AppIconState::Ready(icon.clone()),
            Some(None) => AppIconState::Missing,
            None => AppIconState::Loading,
        }
    }
}

/// One application icon as the UI sees it right now. The two icon-less
/// states render differently on purpose: a resolve still in flight shows a
/// spinner, while an application the platform has no icon for keeps its
/// monogram — otherwise every icon looks broken for a frame.
enum AppIconState {
    Ready(Arc<RenderImage>),
    Loading,
    Missing,
}

enum CatalogLoad {
    Loading,
    Ready(Vec<Application>),
    Failed,
}

/// Search, expansion, and discovery state for the Add App picker.
///
/// The entity owns the one-shot discovery task so closing the app window
/// cancels work whose result no view can consume. Host enumeration stays on
/// GPUI's background executor and never delays the first paint.
pub(crate) struct AppCatalogPicker {
    search: Entity<InputState>,
    expanded: bool,
    load: CatalogLoad,
    preferred_identity: IdentityKind,
    icons: ProfileIconCache,
    /// One in-flight icon resolve per application; dropping the picker
    /// cancels whatever has not landed yet.
    icon_tasks: HashMap<String, Task<()>>,
    /// Keeps the catalog list's scroll position across repaints and feeds
    /// its scrollbar.
    list_scroll: UniformListScrollHandle,
    _search_subscription: Subscription,
    _discovery_task: Task<()>,
}

impl AppCatalogPicker {
    pub(crate) fn new(
        icons: ProfileIconCache,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("Search applications…")));
        let search_subscription = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let discovery_task = cx.spawn(async move |picker, cx| {
            let discovered = cx
                .background_executor()
                .spawn(async {
                    let runtime_identity = appcatalog::foreground_application()
                        .ok()
                        .flatten()
                        .and_then(|app| app.identities.first().map(ApplicationIdentity::kind));
                    appcatalog::applications().map(|applications| (applications, runtime_identity))
                })
                .await;
            picker
                .update(cx, |picker, cx| {
                    match discovered {
                        Ok((applications, runtime_identity)) => {
                            picker.preferred_identity = preferred_identity_kind(runtime_identity);
                            picker.load = CatalogLoad::Ready(applications);
                        }
                        Err(error) => {
                            tracing::warn!(%error, "failed to load application catalog");
                            picker.load = CatalogLoad::Failed;
                        }
                    }
                    cx.notify();
                })
                .ok();
        });

        Self {
            search,
            expanded: false,
            load: CatalogLoad::Loading,
            preferred_identity: preferred_identity_kind(None),
            icons,
            icon_tasks: HashMap::new(),
            list_scroll: UniformListScrollHandle::new(),
            _search_subscription: search_subscription,
            _discovery_task: discovery_task,
        }
    }

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |search, cx| search.set_value("", window, cx));
    }

    /// Start a background resolve for `app`'s icon unless one already
    /// finished or is in flight. Each application resolves independently and
    /// repaints on arrival, so a slow icon never holds up the rest.
    fn ensure_icon(&mut self, app: &str, cx: &mut Context<Self>) {
        if self.icons.icons.borrow().contains_key(app) || self.icon_tasks.contains_key(app) {
            return;
        }
        let app = app.to_string();
        let task = cx.spawn({
            let app = app.clone();
            async move |picker, cx| {
                let icon = cx
                    .background_executor()
                    .spawn({
                        let app = app.clone();
                        async move { crate::platform::app_icon::application_icon(&app) }
                    })
                    .await;
                picker
                    .update(cx, |picker, cx| {
                        picker.icons.icons.borrow_mut().insert(app.clone(), icon);
                        picker.icon_tasks.remove(&app);
                        cx.notify();
                    })
                    .ok();
            }
        });
        self.icon_tasks.insert(app, task);
    }

    fn available_profiles(
        &self,
        observed: &HashSet<String>,
        unavailable: &HashSet<String>,
    ) -> Vec<ProfileChoice> {
        let CatalogLoad::Ready(applications) = &self.load else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        let mut profiles = applications
            .iter()
            .filter_map(|application| {
                let app = identity_for_application(application, observed, self.preferred_identity)?;
                if unavailable.contains(&app) || !seen.insert(app.clone()) {
                    return None;
                }
                Some(ProfileChoice {
                    app,
                    name: application.name.clone(),
                    override_count: 0,
                    persisted: false,
                })
            })
            .collect::<Vec<_>>();
        profiles.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.app.cmp(&right.app))
        });
        profiles
    }
}

fn identity_for_application(
    application: &Application,
    observed: &HashSet<String>,
    preferred: IdentityKind,
) -> Option<String> {
    application
        .identities
        .iter()
        .find(|identity| observed.contains(identity.value()))
        .or_else(|| {
            application
                .identities
                .iter()
                .find(|identity| identity.kind() == preferred)
        })
        // `StartupWMClass` is optional in desktop files. Keep the installed
        // catalog complete on X11/GNOME by falling back to its stable desktop
        // ID. Recently observed candidates always take the exact runtime ID
        // above instead of this registration-time best effort.
        .or_else(|| {
            if preferred != IdentityKind::LinuxStartupWmClass {
                return None;
            }
            application
                .identities
                .iter()
                .find(|identity| identity.kind() == IdentityKind::LinuxDesktopId)
        })
        .map(|identity| identity.value().to_string())
}

fn preferred_identity_kind(runtime: Option<IdentityKind>) -> IdentityKind {
    #[cfg(target_os = "macos")]
    {
        let _ = runtime;
        IdentityKind::MacBundleIdentifier
    }
    #[cfg(target_os = "windows")]
    {
        let _ = runtime;
        IdentityKind::WindowsExecutablePath
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(kind @ (IdentityKind::LinuxStartupWmClass | IdentityKind::LinuxWaylandAppId)) =
            runtime
        {
            return kind;
        }
        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_lowercase();
        let x11 = std::env::var("XDG_SESSION_TYPE").is_ok_and(|session| session == "x11")
            || std::env::var_os("WAYLAND_DISPLAY").is_none();
        if x11 || desktop.contains("gnome") {
            IdentityKind::LinuxStartupWmClass
        } else {
            IdentityKind::LinuxWaylandAppId
        }
    }
}

/// A direct profile switcher. The foreground app may change which profile is
/// active, but never changes which profile this editor has open.
pub(crate) fn profile_scope_bar(
    icons: &ProfileIconCache,
    catalog: &Entity<AppCatalogPicker>,
    cx: &mut App,
) -> Option<ProfileScopeBar> {
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

    let persisted_ids: HashSet<String> = profiles
        .iter()
        .filter(|profile| profile.persisted)
        .map(|profile| profile.app.clone())
        .collect();
    let observed_ids: HashSet<String> = recent_apps.iter().map(|(app, _)| app.clone()).collect();
    let available_recent: Vec<ProfileChoice> = recent_apps
        .iter()
        .filter(|(app, _)| {
            !persisted_ids.contains(app) && editing_app.as_deref() != Some(app.as_str())
        })
        .map(|(app, name)| ProfileChoice {
            app: app.clone(),
            name: name.clone(),
            override_count: 0,
            persisted: false,
        })
        .collect();
    let mut unavailable = persisted_ids;
    unavailable.extend(observed_ids.iter().cloned());
    unavailable.extend(editing_app.iter().cloned());
    catalog.update(cx, |picker, cx| {
        for profile in &profiles {
            picker.ensure_icon(&profile.app, cx);
        }
    });
    let available_catalog = catalog
        .read(cx)
        .available_profiles(&observed_ids, &unavailable);
    let loading = matches!(catalog.read(cx).load, CatalogLoad::Loading);
    let failed = matches!(catalog.read(cx).load, CatalogLoad::Failed);

    Some(ProfileScopeBar {
        editing_app,
        profiles,
        choices: AddAppChoices {
            recent: available_recent,
            applications: available_catalog,
            loading,
            failed,
        },
        catalog: catalog.clone(),
        icons: icons.clone(),
    })
}

/// The profile selector owns the complete profile-switching toolbar and its
/// add/remove menus, including their theme resolution.
#[derive(IntoElement)]
pub(crate) struct ProfileScopeBar {
    editing_app: Option<String>,
    profiles: Vec<ProfileChoice>,
    choices: AddAppChoices,
    catalog: Entity<AppCatalogPicker>,
    icons: ProfileIconCache,
}

impl RenderOnce for ProfileScopeBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        profile_scope_content(
            self.editing_app.as_deref(),
            &self.profiles,
            self.choices,
            self.catalog,
            self.icons,
            pal,
        )
    }
}

/// Profile inheritance and active-app context shown above the device canvas.
pub(crate) fn profile_canvas_status(pal: Palette, cx: &App) -> Option<gpui::Div> {
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
            ),
    )
}

fn profile_scope_content(
    editing_app: Option<&str>,
    profiles: &[ProfileChoice],
    choices: AddAppChoices,
    catalog: Entity<AppCatalogPicker>,
    icons: ProfileIconCache,
    pal: Palette,
) -> impl IntoElement + use<> {
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
                Some(application_mark(
                    icons.state(&profile.app),
                    &profile.name,
                    TAB_ICON_EDGE,
                    pal,
                )),
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
        .child(add_app_popover(choices, catalog, icons, pal))
        .when_some(
            selected_profile.filter(|profile| profile.persisted),
            |row, profile| row.child(profile_options_popover(profile, pal)),
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

fn application_mark(icon: AppIconState, name: &str, edge: f32, pal: Palette) -> gpui::Div {
    let slot = h_flex()
        .size(px(edge))
        .flex_none()
        .items_center()
        .justify_center();
    match icon {
        AppIconState::Ready(icon) => slot.child(img(icon).size(px(edge)).flex_none()),
        AppIconState::Loading => slot
            .child(
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

fn add_app_popover(
    choices: AddAppChoices,
    catalog: Entity<AppCatalogPicker>,
    icons: ProfileIconCache,
    pal: Palette,
) -> impl IntoElement {
    let catalog_on_open = catalog.clone();
    Popover::new("add-app-popover")
        .anchor(Anchor::TopRight)
        // `compact_panel` is the surface; the popover chrome would wrap it in
        // a second padded, differently-rounded box.
        .appearance(false)
        .trigger(
            control_button("add-app-profile")
                .outline()
                .icon(IconName::Plus)
                .label(tr!("Add app")),
        )
        .on_open_change(move |open, window, cx| {
            if *open {
                catalog_on_open.update(cx, |catalog, cx| catalog.clear_search(window, cx));
            }
        })
        .content(move |_state, _window, cx| add_app_content(&choices, &catalog, &icons, pal, cx))
}

fn add_app_content(
    choices: &AddAppChoices,
    catalog: &Entity<AppCatalogPicker>,
    icons: &ProfileIconCache,
    pal: Palette,
    cx: &mut Context<PopoverState>,
) -> gpui::Div {
    let popover = cx.entity().downgrade();
    let catalog_state = catalog.read(cx);
    let search = catalog_state.search.clone();
    let query = search.read(cx).value().trim().to_lowercase();
    let show_applications = catalog_state.expanded || !query.is_empty();
    let list_scroll = catalog_state.list_scroll.clone();
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
            application_row(choice, icon, pal, popover.clone())
        })
        .collect::<Vec<_>>();
    let application_rows = choices
        .applications
        .iter()
        .filter(|choice| profile_matches_query(choice, &query))
        .cloned()
        .collect::<Vec<_>>();
    let no_matches = application_rows.is_empty()
        && !choices.loading
        && !choices.failed
        && (query.is_empty() || recent_rows.is_empty());
    let catalog_for_toggle = catalog.clone();
    let list_catalog = catalog.clone();
    let list_popover = popover.clone();
    let list_len = application_rows.len();
    let application_rows = Arc::new(application_rows);

    compact_panel(pal)
        .w(px(320.))
        .child(title(tr!("Add app profile"), pal))
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
                    .child(tr!("Recent applications")),
            )
        })
        .children(recent_rows)
        .child(div().pt_1().w_full().child(divider(pal)))
        .child(applications_toggle(
            show_applications,
            catalog_for_toggle,
            pal,
        ))
        .when(show_applications && choices.loading, |card| {
            card.child(catalog_message(tr!("Loading applications…"), pal))
        })
        .when(show_applications && choices.failed, |card| {
            card.child(catalog_message(
                tr!("Application catalog unavailable."),
                pal,
            ))
        })
        .when(show_applications && list_len > 0, |card| {
            card.child(catalog_list(
                application_rows,
                list_catalog,
                list_popover,
                &list_scroll,
                pal,
            ))
        })
        .when(show_applications && no_matches, |card| {
            card.child(catalog_message(tr!("No applications found"), pal))
        })
}

/// The scrollable catalog body: a `uniform_list` capped at six rows, with a
/// scrollbar signalling position in the full inventory. Rows resolve their
/// icons as they enter the viewport.
fn catalog_list(
    rows: Arc<Vec<ProfileChoice>>,
    catalog: Entity<AppCatalogPicker>,
    popover: WeakEntity<PopoverState>,
    scroll: &UniformListScrollHandle,
    pal: Palette,
) -> gpui::Div {
    let count = rows.len();
    div()
        .h(px(application_list_height(count)))
        .w_full()
        .child(
            uniform_list("application-catalog-list", count, {
                move |visible_range, _window, cx| {
                    catalog.update(cx, |picker, cx| {
                        visible_range
                            .map(|index| {
                                let choice = rows[index].clone();
                                picker.ensure_icon(&choice.app, cx);
                                let icon = picker.icons.state(&choice.app);
                                application_row(choice, icon, pal, popover.clone())
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
    expanded: bool,
    catalog: Entity<AppCatalogPicker>,
    pal: Palette,
) -> impl IntoElement {
    BaseButton::new("all-applications-toggle")
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
        .child(tr!("All applications"))
        .on_click(move |_event, _window, cx| {
            catalog.update(cx, |catalog, cx| {
                catalog.expanded = !catalog.expanded;
                cx.notify();
            });
        })
}

fn profile_matches_query(choice: &ProfileChoice, query: &str) -> bool {
    query.is_empty()
        || choice.name.to_lowercase().contains(query)
        || choice.app.to_lowercase().contains(query)
}

fn application_row(
    choice: ProfileChoice,
    icon: AppIconState,
    pal: Palette,
    popover: WeakEntity<PopoverState>,
) -> gpui::Div {
    let app = choice.app.clone();
    div().h(px(APP_ROW_H)).child(
        MenuRow::new(format!("catalog-app:{}", choice.app))
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
                AppState::update_bindings(cx, |state| {
                    state.set_editing_app(Some(app.clone()));
                });
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

fn profile_options_popover(profile: ProfileChoice, pal: Palette) -> impl IntoElement {
    Popover::new("profile-options-popover")
        .anchor(Anchor::TopRight)
        // `compact_panel` is the surface here too; see `add_app_popover`.
        .appearance(false)
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
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::rc::Rc;

    use appcatalog::{Application, ApplicationIdentity, IdentityKind};
    use gpui::{
        Context, InteractiveElement as _, IntoElement, Modifiers, ParentElement as _, Render,
        ScrollDelta, ScrollWheelEvent, Styled as _, TestAppContext, TouchPhase, Window, div, point,
        px, uniform_list,
    };
    use gpui_component::button::Button;
    use gpui_component::popover::Popover;

    use super::{APP_ROW_H, MenuRow, compact_panel, friendly_app_name, identity_for_application};
    use crate::ui::theme;

    #[test]
    fn profile_identifiers_have_a_readable_fallback() {
        assert_eq!(friendly_app_name("com.google.Chrome"), "Chrome");
        assert_eq!(friendly_app_name("exe:C:\\Tools\\Zed.exe"), "Zed");
    }

    #[test]
    fn observed_identity_wins_over_the_platform_default() {
        let application = application_with_identities(vec![
            ApplicationIdentity::new(IdentityKind::LinuxWaylandAppId, "org.example.Editor"),
            ApplicationIdentity::new(IdentityKind::LinuxStartupWmClass, "Editor"),
        ]);
        let observed = HashSet::from(["Editor".to_string()]);

        assert_eq!(
            identity_for_application(&application, &observed, IdentityKind::LinuxWaylandAppId,)
                .as_deref(),
            Some("Editor")
        );
    }

    #[test]
    fn unobserved_application_uses_the_active_identity_namespace() {
        let application = application_with_identities(vec![
            ApplicationIdentity::new(IdentityKind::LinuxWaylandAppId, "org.example.Editor"),
            ApplicationIdentity::new(IdentityKind::LinuxStartupWmClass, "Editor"),
        ]);

        assert_eq!(
            identity_for_application(
                &application,
                &HashSet::new(),
                IdentityKind::LinuxWaylandAppId,
            )
            .as_deref(),
            Some("org.example.Editor")
        );
    }

    #[test]
    fn linux_desktop_id_keeps_apps_without_startup_class_available() {
        let application = application_with_identities(vec![ApplicationIdentity::new(
            IdentityKind::LinuxDesktopId,
            "org.example.Editor",
        )]);

        assert_eq!(
            identity_for_application(
                &application,
                &HashSet::new(),
                IdentityKind::LinuxStartupWmClass,
            )
            .as_deref(),
            Some("org.example.Editor")
        );
    }

    fn application_with_identities(identities: Vec<ApplicationIdentity>) -> Application {
        Application {
            name: "Editor".into(),
            identities,
            executable: None,
            registration: "editor.desktop".into(),
            icon: None,
        }
    }

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
}
