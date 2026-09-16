//! Per-application profiles shared by device feature editors.

mod catalog;
mod picker;
mod shell;

use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use gpui::{App, Entity, ParentElement, Styled, Window, div};
use gpui_component::{WindowExt as _, button::ButtonVariant, dialog::DialogButtonProps, h_flex};
use openlogi_core::binding::{ActionRingConfig, ActionRingLayout, ActionRingSlot};

pub(crate) use self::catalog::{AppCatalogPicker, AppIconState, ProfileIconCache};
pub(crate) use self::picker::application_popover;
use self::shell::ProfileScopeShell;
use crate::state::AppState;
use crate::ui::theme::{self, Typography as _};

#[derive(Clone)]
pub(super) struct ProfileChoice {
    pub(super) app: String,
    pub(super) launch_target: Option<String>,
    pub(super) name: String,
    pub(super) override_count: usize,
    pub(super) persisted: bool,
}

pub(super) enum CatalogPresentation {
    Loading,
    Ready(Vec<ProfileChoice>),
    Failed,
}

pub(super) struct AddAppChoices {
    pub(super) recent: Vec<ProfileChoice>,
    pub(super) catalog: CatalogPresentation,
}

pub(super) struct ProfileScopeModel {
    pub(super) editing_app: Option<String>,
    pub(super) profiles: Vec<ProfileChoice>,
    pub(super) choices: AddAppChoices,
}

type SelectProfile = dyn Fn(Option<String>, &mut App);
type RemoveProfile = dyn Fn(ProfileChoice, &mut Window, &mut App);

/// Feature-owned behavior invoked by the profile selector shell.
#[derive(Clone)]
pub(super) struct ProfileScopeActions {
    select: Rc<SelectProfile>,
    remove: Rc<RemoveProfile>,
}

impl ProfileScopeActions {
    fn new(
        select: impl Fn(Option<String>, &mut App) + 'static,
        remove: impl Fn(ProfileChoice, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            select: Rc::new(select),
            remove: Rc::new(remove),
        }
    }

    pub(super) fn select(&self, app: Option<String>, cx: &mut App) {
        (self.select)(app, cx);
    }

    pub(super) fn remove(&self, profile: ProfileChoice, window: &mut Window, cx: &mut App) {
        (self.remove)(profile, window, cx);
    }
}

/// Build the Buttons workspace's profile selector.
pub(crate) fn button_profile_scope_bar(
    icons: &ProfileIconCache,
    catalog: &Entity<AppCatalogPicker>,
    cx: &mut App,
) -> Option<ProfileScopeShell> {
    let state = AppState::try_read(cx)?;
    if !state.current_device_is_persistent() {
        return None;
    }
    let editing_app = state.editing_app().map(str::to_string);
    let profiles: Vec<ProfileChoice> = state
        .app_profiles()
        .map(|(app, override_count)| ProfileChoice {
            app: app.to_string(),
            launch_target: None,
            name: state
                .recent_app_name(app)
                .map_or_else(|| friendly_app_name(app), str::to_string),
            override_count,
            persisted: true,
        })
        .collect();
    let recent_apps: Vec<(String, String)> = state
        .recent_apps()
        .map(|(app, name)| (app.to_string(), name.to_string()))
        .collect();
    let model = profile_scope_model(editing_app, profiles, &recent_apps, catalog, cx);
    let actions = ProfileScopeActions::new(
        |app, cx| {
            AppState::update_bindings(cx, |state| {
                state.set_editing_app(app);
            });
        },
        |profile, window, cx| open_button_remove_confirmation(window, cx, &profile),
    );

    Some(ProfileScopeShell::new(
        "button-profile",
        model,
        catalog.clone(),
        icons.clone(),
        actions,
    ))
}

/// Build the Actions Ring workspace's independent profile selector.
pub(crate) fn action_ring_profile_scope_bar(
    icons: &ProfileIconCache,
    catalog: &Entity<AppCatalogPicker>,
    cx: &mut App,
) -> Option<ProfileScopeShell> {
    let state = AppState::try_read(cx)?;
    if !state.current_device_is_persistent() {
        return None;
    }
    let editing_app = state.editing_action_ring_app().map(str::to_string);
    let ring = state.current_action_ring();
    let profiles: Vec<ProfileChoice> = ring
        .per_app
        .iter()
        .map(|(app, layout)| ProfileChoice {
            app: app.clone(),
            launch_target: None,
            name: state
                .recent_app_name(app)
                .map_or_else(|| friendly_app_name(app), str::to_string),
            override_count: action_ring_override_count(&ring, layout),
            persisted: true,
        })
        .collect();
    let recent_apps: Vec<(String, String)> = state
        .recent_apps()
        .map(|(app, name)| (app.to_string(), name.to_string()))
        .collect();
    let model = profile_scope_model(editing_app, profiles, &recent_apps, catalog, cx);
    let actions = ProfileScopeActions::new(
        |app, cx| {
            AppState::update(cx, |state, cx| {
                let key = state
                    .current_record()
                    .map(crate::state::DeviceRecord::device_key);
                state.set_editing_action_ring_app(app);
                if let Some(key) = key {
                    cx.emit(crate::state::StateEvent::BindingsChanged(key));
                }
            });
        },
        |profile, window, cx| open_action_ring_remove_confirmation(window, cx, &profile),
    );

    Some(ProfileScopeShell::new(
        "action-ring-profile",
        model,
        catalog.clone(),
        icons.clone(),
        actions,
    ))
}

fn profile_scope_model(
    editing_app: Option<String>,
    mut profiles: Vec<ProfileChoice>,
    recent_apps: &[(String, String)],
    catalog: &Entity<AppCatalogPicker>,
    cx: &mut App,
) -> ProfileScopeModel {
    if let Some(app) = editing_app.as_deref()
        && !profiles.iter().any(|profile| profile.app == app)
    {
        profiles.push(ProfileChoice {
            app: app.to_string(),
            launch_target: None,
            name: recent_apps
                .iter()
                .find(|(identifier, _)| identifier == app)
                .map_or_else(|| friendly_app_name(app), |(_, name)| name.clone()),
            override_count: 0,
            persisted: false,
        });
    }
    profiles.sort_by_key(|profile| profile.name.to_lowercase());

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
            launch_target: None,
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
    let catalog_presentation = catalog
        .read(cx)
        .available_profiles(&observed_ids, &unavailable);
    let choices = AddAppChoices {
        recent: available_recent,
        catalog: catalog_presentation,
    };
    ProfileScopeModel {
        editing_app,
        profiles,
        choices,
    }
}

fn action_ring_override_count(ring: &ActionRingConfig, layout: &ActionRingLayout) -> usize {
    ActionRingSlot::ALL
        .into_iter()
        .filter(|slot| layout.slots.get(slot) != ring.default.slots.get(slot))
        .count()
}

/// Profile inheritance and active-app context shown above the device canvas.
pub(crate) fn profile_canvas_status(cx: &App) -> Option<gpui::Div> {
    let pal = theme::palette(cx);
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
        .map_or_else(|| tr!("common.default"), gpui::SharedString::from);

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
                    .child(tr!("profiles.active_profile_value", profile => active)),
            ),
    )
}

fn profile_summary(editing_app: Option<&str>, override_count: usize) -> gpui::SharedString {
    let Some(app) = editing_app else {
        return tr!("profiles.default_bindings_description");
    };
    match override_count {
        0 => tr!(
            "profiles.app_profile_no_overrides",
            app => app
        ),
        1 => tr!(
            "profiles.app_profile_single_override",
            app => app
        ),
        count => tr!(
            "profiles.app_profile_override_count",
            app => app,
            count => count
        ),
    }
}

fn open_button_remove_confirmation(window: &mut Window, cx: &mut App, profile: &ProfileChoice) {
    let question = remove_profile_question(profile);
    window.open_alert_dialog(cx, move |alert, _, _| {
        alert
            .title(question.clone())
            .description(tr!("profiles.remove_profile_description"))
            .button_props(
                DialogButtonProps::default()
                    .ok_text(tr!("profiles.remove_profile"))
                    .ok_variant(ButtonVariant::Danger)
                    .cancel_text(tr!("common.cancel"))
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

fn open_action_ring_remove_confirmation(
    window: &mut Window,
    cx: &mut App,
    profile: &ProfileChoice,
) {
    let question = remove_profile_question(profile);
    window.open_alert_dialog(cx, move |alert, _, _| {
        alert
            .title(question.clone())
            .button_props(
                DialogButtonProps::default()
                    .ok_text(tr!("profiles.remove_profile"))
                    .ok_variant(ButtonVariant::Danger)
                    .cancel_text(tr!("common.cancel"))
                    .show_cancel(true),
            )
            .on_ok(move |_event, _window, cx| {
                AppState::update(cx, |state, cx| {
                    let key = state
                        .current_record()
                        .map(crate::state::DeviceRecord::device_key);
                    state.remove_editing_action_ring_profile();
                    if let Some(key) = key {
                        cx.emit(crate::state::StateEvent::BindingsChanged(key));
                    }
                });
                true
            })
    });
}

fn remove_profile_question(profile: &ProfileChoice) -> gpui::SharedString {
    match profile.override_count {
        1 => tr!(
            "profiles.remove_app_profile_single_override",
            app => profile.name.clone()
        ),
        count => tr!(
            "profiles.remove_app_profile_multiple_overrides",
            app => profile.name.clone(),
            count => count
        ),
    }
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
