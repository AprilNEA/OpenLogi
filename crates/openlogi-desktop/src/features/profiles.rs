//! Per-application profiles shared by device feature editors.

mod catalog;
mod picker;
mod shell;

use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use gpui::{App, Entity, ParentElement, Styled, Window, div};
use gpui_component::{WindowExt as _, button::ButtonVariant, dialog::DialogButtonProps, h_flex};

pub(crate) use self::catalog::{AppCatalogPicker, ProfileIconCache};
use self::shell::ProfileScopeShell;
use crate::state::{AppState, DeviceKey};
use crate::ui::theme::{self, Typography as _};

#[derive(Clone)]
pub(super) struct ProfileChoice {
    pub(super) app: String,
    pub(super) name: String,
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
type ChangeProfile = dyn Fn(ProfileChoice, &mut Window, &mut App);

/// Feature-owned behavior invoked by the profile selector shell.
#[derive(Clone)]
pub(super) struct ProfileScopeActions {
    select: Rc<SelectProfile>,
    reset: Rc<ChangeProfile>,
    remove: Rc<ChangeProfile>,
    remove_all: Rc<ChangeProfile>,
}

impl ProfileScopeActions {
    fn new(
        select: impl Fn(Option<String>, &mut App) + 'static,
        reset: impl Fn(ProfileChoice, &mut Window, &mut App) + 'static,
        remove: impl Fn(ProfileChoice, &mut Window, &mut App) + 'static,
        remove_all: impl Fn(ProfileChoice, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            select: Rc::new(select),
            reset: Rc::new(reset),
            remove: Rc::new(remove),
            remove_all: Rc::new(remove_all),
        }
    }

    pub(super) fn select(&self, app: Option<String>, cx: &mut App) {
        (self.select)(app, cx);
    }

    pub(super) fn reset(&self, profile: ProfileChoice, window: &mut Window, cx: &mut App) {
        (self.reset)(profile, window, cx);
    }

    pub(super) fn remove(&self, profile: ProfileChoice, window: &mut Window, cx: &mut App) {
        (self.remove)(profile, window, cx);
    }

    pub(super) fn remove_all(&self, profile: ProfileChoice, window: &mut Window, cx: &mut App) {
        (self.remove_all)(profile, window, cx);
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
    let key = state.current_record()?.device_key();
    let reset_key = key.clone();
    let remove_all_key = key.clone();
    let editing_app = state.editing_app().map(str::to_string);
    let profiles: Vec<ProfileChoice> = state
        .app_profiles()
        .map(|(app, _)| ProfileChoice {
            app: app.to_string(),
            name: state
                .recent_app_name(app)
                .map_or_else(|| friendly_app_name(app), str::to_string),
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
            AppState::apply(cx, |state| state.set_editing_app(app));
        },
        move |profile, window, cx| {
            ProfileCommand::ResetButtons.confirm(&reset_key, &profile, window, cx);
        },
        move |profile, window, cx| {
            ProfileCommand::RemoveButtons.confirm(&key, &profile, window, cx);
        },
        move |profile, window, cx| {
            ProfileCommand::RemoveAll.confirm(&remove_all_key, &profile, window, cx);
        },
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
    let key = state.current_record()?.device_key();
    let reset_key = key.clone();
    let remove_all_key = key.clone();
    let editing_app = state.editing_action_ring_app().map(str::to_string);
    let ring = state.current_action_ring();
    let profiles: Vec<ProfileChoice> = ring
        .per_app
        .keys()
        .map(|app| ProfileChoice {
            app: app.clone(),
            name: state
                .recent_app_name(app)
                .map_or_else(|| friendly_app_name(app), str::to_string),
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
            AppState::apply(cx, |state| state.set_editing_action_ring_app(app));
        },
        move |profile, window, cx| {
            ProfileCommand::ResetRing.confirm(&reset_key, &profile, window, cx);
        },
        move |profile, window, cx| {
            ProfileCommand::RemoveRing.confirm(&key, &profile, window, cx);
        },
        move |profile, window, cx| {
            ProfileCommand::RemoveAll.confirm(&remove_all_key, &profile, window, cx);
        },
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
            name: recent_apps
                .iter()
                .find(|(identifier, _)| identifier == app)
                .map_or_else(|| friendly_app_name(app), |(_, name)| name.clone()),
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
            name: name.clone(),
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

#[derive(Clone, Copy)]
enum ProfileCommand {
    ResetButtons,
    RemoveButtons,
    ResetRing,
    RemoveRing,
    RemoveAll,
}

impl ProfileCommand {
    fn apply(self, key: &DeviceKey, app: &str, cx: &mut App) {
        AppState::apply(cx, |state| match self {
            Self::ResetButtons => state.reset_app_profile(key, app),
            Self::RemoveButtons => state.remove_app_profile(key, app),
            Self::ResetRing => state.reset_action_ring_profile(key, app),
            Self::RemoveRing => state.remove_action_ring_profile(key, app),
            Self::RemoveAll => state.remove_all_app_profiles(key, app),
        });
    }

    fn confirm(self, key: &DeviceKey, profile: &ProfileChoice, window: &mut Window, cx: &mut App) {
        // A draft in this editor may still have saved settings in the other.
        if !profile.persisted && !matches!(self, Self::RemoveAll) {
            self.apply(key, &profile.app, cx);
            return;
        }
        let key = key.clone();
        let app = profile.app.clone();
        let name = profile.name.clone();
        let reset = matches!(self, Self::ResetButtons | Self::ResetRing);
        window.open_alert_dialog(cx, move |alert, _, _| {
            let key = key.clone();
            let app = app.clone();
            let question = if matches!(self, Self::RemoveAll) {
                tr!("profiles.remove_all_profiles_question", app => name.clone())
            } else if reset {
                tr!("profiles.reset_profile_question", app => name.clone())
            } else {
                tr!("profiles.remove_profile_question", app => name.clone())
            };
            let description = match self {
                Self::ResetButtons => tr!("profiles.reset_buttons_description"),
                Self::RemoveButtons => tr!("profiles.remove_buttons_description"),
                Self::ResetRing => tr!("profiles.reset_ring_description"),
                Self::RemoveRing => tr!("profiles.remove_ring_description"),
                Self::RemoveAll => tr!("profiles.remove_all_profiles_description"),
            };
            alert
                .title(question)
                .description(description)
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(if matches!(self, Self::RemoveAll) {
                            tr!("profiles.remove_all_profiles")
                        } else if reset {
                            tr!("profiles.reset_profile")
                        } else {
                            tr!("profiles.remove_profile")
                        })
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text(tr!("common.cancel"))
                        .show_cancel(true),
                )
                .on_ok(move |_event, _window, cx| {
                    self.apply(&key, &app, cx);
                    true
                })
        });
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
