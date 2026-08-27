//! Installed-application discovery and icon state for profile UI.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use appcatalog::{Application, ApplicationIdentity, IdentityKind};
use gpui::{
    AppContext as _, Context, Entity, RenderImage, Subscription, Task, UniformListScrollHandle,
    Window,
};
use gpui_component::input::{InputEvent, InputState};

use super::{CatalogPresentation, ProfileChoice};

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
    pub(super) fn state(&self, app: &str) -> AppIconState {
        match self.icons.borrow().get(app) {
            Some(Some(icon)) => AppIconState::Ready(icon.clone()),
            Some(None) => AppIconState::Missing,
            None => AppIconState::Loading,
        }
    }
}

/// One application icon as the UI sees it right now. The two icon-less states
/// render differently so an in-flight resolve does not look like a permanent
/// missing icon.
pub(super) enum AppIconState {
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
    /// Keeps the catalog list's scroll position across repaints and feeds its
    /// scrollbar.
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

    pub(super) fn search(&self) -> Entity<InputState> {
        self.search.clone()
    }

    pub(super) const fn expanded(&self) -> bool {
        self.expanded
    }

    pub(super) fn toggle_expanded(&mut self, cx: &mut Context<Self>) {
        self.expanded = !self.expanded;
        cx.notify();
    }

    pub(super) fn list_scroll(&self) -> UniformListScrollHandle {
        self.list_scroll.clone()
    }

    pub(super) fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |search, cx| search.set_value("", window, cx));
    }

    /// Start a background resolve for `app`'s icon unless one already
    /// finished or is in flight. Each application resolves independently and
    /// repaints on arrival, so a slow icon never holds up the rest.
    pub(super) fn ensure_icon(&mut self, app: &str, cx: &mut Context<Self>) {
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

    pub(super) fn icon_state(&self, app: &str) -> AppIconState {
        self.icons.state(app)
    }

    pub(super) fn available_profiles(
        &self,
        observed: &HashSet<String>,
        unavailable: &HashSet<String>,
    ) -> CatalogPresentation {
        let applications = match &self.load {
            CatalogLoad::Loading => return CatalogPresentation::Loading,
            CatalogLoad::Ready(applications) => applications,
            CatalogLoad::Failed => return CatalogPresentation::Failed,
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
        CatalogPresentation::Ready(profiles)
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use appcatalog::{Application, ApplicationIdentity, IdentityKind};

    use super::identity_for_application;

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
}
