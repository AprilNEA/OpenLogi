//! Updates settings page.

use super::{
    App, AppState, Button, ButtonGroup, Disableable, Entity, FontWeight, IconName,
    InteractiveElement as _, ParentElement, RELEASES_URL, Selectable, SettingField, SettingGroup,
    SettingItem, SettingPage, Sizable, StatefulInteractiveElement as _, Styled, Tag, UpdateStatus,
    Updater, div, h_flex, img, px, v_flex,
};
use crate::platform::installation::{HomebrewCask, Installation, InstallationSource, LinuxPackage};
use crate::platform::updater::linux::{
    self, InstallOffer, LinuxUpdate, LinuxUpdateError, LinuxUpdateStatus,
};
use crate::ui::theme::FocusRingStyle as _;
use crate::ui::theme::PressableStyle as _;
use crate::ui::theme::SelectableStyle as _;
use crate::ui::theme::Typography as _;
use gpui_kit::base::Link;
use gpui_kit::component::ActiveTheme as _;
use openlogi_core::update_feed::UpdateChannel;

/// The Updates page: a hero card with the running build, its update status, and
/// the contextual check / install / restart action; the opt-in auto-check and
/// auto-install switches and the update channel; installation ownership and
/// where updates come from.
pub(super) fn updates_page(updater: Entity<Updater>) -> SettingPage {
    let hero = SettingGroup::new().item(SettingItem::render(move |_, _, cx| {
        update_hero(&updater, cx)
    }));

    let toggles = SettingGroup::new()
        .item(
            SettingItem::new(
                tr!("app.check_for_updates_setting"),
                SettingField::switch(
                    |cx| AppState::try_read(cx).is_some_and(|s| s.app_settings().check_for_updates),
                    |enabled, cx| {
                        AppState::apply(cx, |state| state.commit_check_for_updates(enabled));
                    },
                ),
            )
            .description(tr!("app.update_check_frequency_description")),
        )
        .item(
            SettingItem::new(
                tr!("updates.automatically_download_and_install"),
                SettingField::switch(
                    |cx| {
                        AppState::try_read(cx)
                            .is_some_and(|s| s.app_settings().auto_install_updates)
                    },
                    crate::platform::updater::set_auto_install_updates,
                ),
            )
            .description(tr!("updates.automatic_update_description")),
        )
        .item(
            SettingItem::new(
                tr!("updates.update_channel"),
                SettingField::render(|_, _, cx| channel_segment(cx)),
            )
            .description(tr!("updates.update_channel_description")),
        );

    let source = SettingGroup::new()
        .item(SettingItem::new(
            tr!("updates.installation_source"),
            SettingField::render(|_, _, cx| installation_value(cx)),
        ))
        .item(SettingItem::new(
            tr!("updates.update_source"),
            SettingField::render(|_, _, cx| release_link(cx)),
        ))
        .gap_2()
        .footer(|_, cx| {
            // Kit aligns footers with the surface edge; this note follows the
            // filled group's setting labels and their standard content inset.
            div().px_4().child(
                div()
                    .text_caption()
                    .text_color(crate::ui::theme::palette(cx).text_muted)
                    .debug_selector(|| "update-connection-policy".into())
                    .child(tr!("updates.update_connection_policy")),
            )
        });
    SettingPage::new(tr!("updates.updates"))
        .icon(IconName::ArrowDown)
        .resettable(false)
        .description(tr!("updates.update_network_privacy_description"))
        .group(hero)
        .group(toggles)
        .group(source)
}

/// The hero's status pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pill {
    UpToDate,
    Available,
    Ready,
    Failed,
}

impl Pill {
    /// The debug selector the rendered pill carries.
    fn selector(self) -> &'static str {
        match self {
            Self::UpToDate => "update-pill-up-to-date",
            Self::Available => "update-pill-available",
            Self::Ready => "update-pill-ready",
            Self::Failed => "update-pill-failed",
        }
    }

    fn tag(self) -> Tag {
        match self {
            Self::UpToDate => Tag::success().child(tr!("updates.up_to_date")),
            Self::Available => Tag::info().child(tr!("updates.update_available")),
            Self::Ready => Tag::success().child(tr!("updates.update_ready")),
            Self::Failed => Tag::danger().child(tr!("updates.update_failed")),
        }
    }
}

/// The hero's one contextual action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeroAction {
    Check,
    Install,
    Restart,
}

impl HeroAction {
    /// The button's element id, which is also its debug selector.
    fn id(self) -> &'static str {
        match self {
            Self::Check => "update-check",
            Self::Install => "update-install",
            Self::Restart => "update-restart",
        }
    }

    fn label(self) -> gpui::SharedString {
        match self {
            Self::Check => tr!("updates.check_for_updates_action"),
            Self::Install => tr!("updates.download_install"),
            Self::Restart => tr!("updates.restart_to_update"),
        }
    }
}

/// The update path the hero reports, and so the one its action drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeroPath {
    /// The shared [`Updater`]: Sparkle or the built-in engine.
    Engine,
    /// The Linux package update path ([`linux`]).
    Linux,
}

/// What the hero shows next to the build name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HeroContent {
    path: HeroPath,
    pill: Option<Pill>,
    /// The status line; `None` shows the followed channel instead.
    message: Option<gpui::SharedString>,
    /// Work is in flight: the action is disabled until it settles.
    busy: bool,
    action: HeroAction,
}

/// The hero for this app's update path.
///
/// On Linux the engine's manifest deliberately carries no Linux asset —
/// Linux installs update through the OS package manager, not the engine (see
/// `xtask/src/commands/release/latest_json.rs`) — so whatever the engine
/// reports there is not this install's update state; the Linux path's is.
/// The path is picked by whether the Linux path is installed (only ever on
/// Linux, see [`linux::install_path`]), not by the host OS, so both are
/// tested on every host.
fn hero_content(updater: &Entity<Updater>, cx: &App) -> HeroContent {
    match cx.try_global::<LinuxUpdate>() {
        Some(linux) => linux_hero(
            linux,
            cx.try_global::<Installation>()
                .copied()
                .unwrap_or(Installation::Detecting),
        ),
        None => engine_hero(updater.read(cx).status()),
    }
}

fn engine_hero(status: &UpdateStatus) -> HeroContent {
    let pill = match status {
        UpdateStatus::UpToDate => Some(Pill::UpToDate),
        UpdateStatus::Available(_) => Some(Pill::Available),
        UpdateStatus::Staged(_) => Some(Pill::Ready),
        UpdateStatus::Errored(_) => Some(Pill::Failed),
        _ => None,
    };
    let message = match status {
        UpdateStatus::Idle | UpdateStatus::UpToDate => None,
        UpdateStatus::Checking => Some(tr!("updates.checking_for_updates")),
        UpdateStatus::Available(v) => Some(tr!("updates.update_version_available", version => v)),
        UpdateStatus::Downloading { downloaded, total } => Some(match total {
            Some(t) if *t > 0 => {
                tr!("updates.update_downloading_percent", percent => (*downloaded * 100 / *t).to_string())
            }
            // Sparkle reports no byte counts; its own window shows the progress.
            _ if *downloaded == 0 => tr!("updates.update_downloading"),
            _ => {
                tr!("updates.update_downloading_size", size => (*downloaded / 1_048_576).to_string())
            }
        }),
        UpdateStatus::Installing => Some(tr!("updates.installing")),
        UpdateStatus::Staged(v) => Some(tr!("updates.update_version_ready", version => v)),
        UpdateStatus::Errored(e) => Some(tr!("updates.update_failed_message", error => e.clone())),
    };
    let action = match status {
        UpdateStatus::Available(_) => HeroAction::Install,
        UpdateStatus::Staged(_) => HeroAction::Restart,
        _ => HeroAction::Check,
    };
    HeroContent {
        path: HeroPath::Engine,
        pill,
        message,
        busy: matches!(
            status,
            UpdateStatus::Checking | UpdateStatus::Downloading { .. } | UpdateStatus::Installing
        ),
        action,
    }
}

fn linux_hero(linux: &LinuxUpdate, installation: Installation) -> HeroContent {
    let (pill, message, busy, action) = match linux.status() {
        LinuxUpdateStatus::Idle => (None, None, false, HeroAction::Check),
        LinuxUpdateStatus::Checking(_) => (
            None,
            Some(tr!("updates.checking_for_updates")),
            true,
            HeroAction::Check,
        ),
        LinuxUpdateStatus::UpToDate => (Some(Pill::UpToDate), None, false, HeroAction::Check),
        LinuxUpdateStatus::Available(release) => {
            let version = &release.version;
            let (message, action) = match linux.offer(installation) {
                Some(InstallOffer::Ready(_)) => (
                    tr!("updates.update_version_available", version => version),
                    HeroAction::Install,
                ),
                Some(InstallOffer::NotPublished) => (
                    tr!("updates.update_version_available_package_missing", version => version),
                    HeroAction::Check,
                ),
                // Whatever installed this copy updates it.
                Some(InstallOffer::PackageManager) => (
                    tr!("updates.update_version_available_package_manager", version => version),
                    HeroAction::Check,
                ),
                // An open window redraws once detection settles.
                Some(InstallOffer::Detecting) | None => (
                    tr!("updates.update_version_available", version => version),
                    HeroAction::Check,
                ),
            };
            (Some(Pill::Available), Some(message), false, action)
        }
        LinuxUpdateStatus::Installing { .. } => (
            None,
            Some(tr!("updates.installing")),
            true,
            HeroAction::Check,
        ),
        LinuxUpdateStatus::Staged { version } => (
            Some(Pill::Ready),
            Some(tr!("updates.update_version_ready", version => version)),
            false,
            HeroAction::Restart,
        ),
        LinuxUpdateStatus::Errored(error) => (
            Some(Pill::Failed),
            Some(tr!("updates.update_failed_message", error => linux_error_message(*error))),
            false,
            HeroAction::Check,
        ),
    };
    HeroContent {
        path: HeroPath::Linux,
        pill,
        message,
        busy,
        action,
    }
}

/// The translated reason a Linux check or install failed. The technical
/// detail went to the log (see [`LinuxUpdateError`]).
fn linux_error_message(error: LinuxUpdateError) -> gpui::SharedString {
    match error {
        LinuxUpdateError::Check => tr!("updates.update_error_check"),
        LinuxUpdateError::MissingTool(program) => {
            tr!("updates.update_error_missing_tool", program => program)
        }
        LinuxUpdateError::Download => tr!("updates.update_error_download"),
        LinuxUpdateError::Verification => tr!("updates.update_error_verification"),
        LinuxUpdateError::Authorization => tr!("updates.update_error_authorization"),
        LinuxUpdateError::Install => tr!("updates.update_error_install"),
    }
}

/// Run the hero's action on the path that offered it.
fn run_hero_action(path: HeroPath, action: HeroAction, updater: &Entity<Updater>, cx: &mut App) {
    match (path, action) {
        (HeroPath::Linux, HeroAction::Check) => linux::check(cx),
        (HeroPath::Linux, HeroAction::Install) => linux::install(cx),
        (HeroPath::Linux, HeroAction::Restart) => linux::restart(cx),
        (HeroPath::Engine, HeroAction::Check) => updater.update(cx, Updater::check),
        (HeroPath::Engine, HeroAction::Install) => {
            updater.update(cx, Updater::download_and_install);
        }
        (HeroPath::Engine, HeroAction::Restart) => updater.update(cx, Updater::restart),
    }
}

/// The Updates hero row: logo, name + version, a status pill, the live status
/// message (or channel), and the one contextual action button.
fn update_hero(updater: &Entity<Updater>, cx: &mut App) -> gpui::Div {
    let pal = crate::ui::theme::palette(cx);
    let channel = crate::platform::updater::channel_in_use(cx);
    let HeroContent {
        path,
        pill,
        message,
        busy,
        action,
    } = hero_content(updater, cx);
    let updater = updater.clone();
    let button = Button::new(action.id())
        .outline()
        .label(action.label())
        .debug_selector(move || action.id().into())
        .on_click(move |_, _, cx| run_hero_action(path, action, &updater, cx))
        .disabled(busy);

    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_4()
        .child(
            // The left block yields and ellipsizes; the action button never
            // shrinks — mirrors the library's own SettingItem rows, which
            // otherwise protect themselves the same way. Without this a long
            // status line (or a wide UI font) shoves the button past the
            // window edge.
            h_flex()
                .items_center()
                .gap_3()
                .flex_1()
                .min_w_0()
                .child(img(crate::app_assets::LOGO).w(px(52.)).h(px(52.)))
                .child(
                    v_flex()
                        .gap_1()
                        .min_w_0()
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(concat!("OpenLogi ", env!("CARGO_PKG_VERSION"))),
                                )
                                .children(pill.map(|pill| {
                                    div()
                                        .debug_selector(move || pill.selector().into())
                                        .child(pill.tag().small().rounded_full())
                                })),
                        )
                        .child(
                            div()
                                .text_caption()
                                .text_color(pal.text_muted)
                                .truncate()
                                .debug_selector(|| "update-status-message".into())
                                .child(message.unwrap_or_else(|| channel_label(channel))),
                        ),
                ),
        )
        .child(div().flex_shrink_0().child(button))
}

/// The hero line's name for the channel this launch follows.
fn channel_label(channel: UpdateChannel) -> gpui::SharedString {
    match channel {
        UpdateChannel::Stable => tr!("updates.stable_channel"),
        UpdateChannel::Beta => tr!("updates.beta_channel"),
    }
}

/// Stable / Beta. The selected one is the channel the settings make the next
/// launch follow, picked or not; clicking one picks it.
///
/// Each button commits its own channel. `ButtonGroup::on_click` runs only on
/// a pointer click on the group, so Enter and Space on a focused button would
/// not reach it.
fn channel_segment(cx: &App) -> ButtonGroup {
    let current = crate::platform::updater::followed_channel(
        AppState::try_read(cx).map(AppState::app_settings),
    );
    ButtonGroup::new("update-channel")
        .outline()
        .children(UpdateChannel::ALL.map(|channel| {
            let (id, label) = match channel {
                UpdateChannel::Stable => ("update-channel-stable", tr!("updates.channel_stable")),
                UpdateChannel::Beta => ("update-channel-beta", tr!("updates.channel_beta")),
            };
            Button::new(id)
                .label(label)
                .selected(current == channel)
                .selected_segment(current == channel, cx)
                .debug_selector(move || id.into())
                .on_click(move |_, _, cx| {
                    AppState::apply(cx, |state| state.commit_update_channel(channel));
                })
        }))
}

/// Read the published snapshot only; rendering must never probe the filesystem.
fn installation_value(cx: &App) -> gpui::Stateful<gpui::Div> {
    let label = installation_label(*cx.global::<Installation>());
    div()
        .id("installation-source")
        .role(gpui::Role::Status)
        .aria_label(label.clone())
        .text_right()
        .text_sm()
        .text_color(crate::ui::theme::palette(cx).text_muted)
        .debug_selector(|| "installation-source-value".into())
        .child(label)
}

fn installation_label(installation: Installation) -> gpui::SharedString {
    match installation {
        Installation::Detecting => tr!("updates.installation_detecting"),
        Installation::Detected(source) => match source {
            InstallationSource::Homebrew(HomebrewCask::Official) => "Homebrew (openlogi)".into(),
            InstallationSource::Homebrew(HomebrewCask::Latest) => {
                "Homebrew (openlogi@latest)".into()
            }
            InstallationSource::LinuxPackage(LinuxPackage::Deb) => "DEB (dpkg)".into(),
            InstallationSource::LinuxPackage(LinuxPackage::Rpm) => "RPM".into(),
            InstallationSource::LinuxPackage(LinuxPackage::Arch) => "Arch Linux (pacman)".into(),
            InstallationSource::Nix => "Nix".into(),
            InstallationSource::WindowsMsi => tr!("updates.installation_windows_msi"),
            InstallationSource::WindowsPortable => tr!("updates.installation_windows_portable"),
            InstallationSource::MacAppBundle => tr!("updates.installation_macos_bundle"),
            InstallationSource::Unknown => tr!("updates.installation_unknown"),
        },
    }
}

/// The external release-notes field, using the standard setting-row type size.
fn release_link(cx: &App) -> Link {
    let pal = crate::ui::theme::palette(cx);
    let link_color = cx.theme().link;
    Link::new("update-changelog")
        .href(RELEASES_URL)
        .accessibility_label(tr!("updates.view_changelog"))
        .open_with(|href, _, _, cx| cx.open_url(href))
        .flex()
        .items_center()
        .flex_shrink_0()
        .gap_1()
        .py_1()
        .text_sm()
        .text_color(link_color)
        .rounded(pal.control_radius)
        .cursor_pointer()
        .pressable_fill(false, pal)
        .focus_visible(move |style| style.focus_ring(pal))
        .debug_selector(|| "update-source-link".into())
        .child(div().underline().child("GitHub Releases"))
        .child(gpui_kit::component::Icon::new(IconName::ExternalLink).size_3p5())
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui::{
        AppContext as _, Context, IntoElement, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers,
        ParentElement as _, Render, ScrollDelta, ScrollEvent, Styled as _, TestAppContext,
        VisualTestContext, Window, div, point,
    };
    use openlogi_core::config::{Config, UiScale};

    use super::*;
    use crate::platform::updater::linux::release_check::{
        LatestRelease, LinuxUpdateCheck, ReleaseAsset,
    };
    use crate::services::{assets::AssetResolver, i18n::LOCALE_LOCK};
    use crate::state::Sources;
    use crate::windows::settings::{SettingsPage, SettingsView};

    const RPM: Installation =
        Installation::Detected(InstallationSource::LinuxPackage(LinuxPackage::Rpm));

    /// Every asset an RPM install of 99.0.0 needs; `{arch}` is this host's.
    const FULL_RPM: &[&str] = &[
        "openlogi-v99.0.0-linux-{arch}.rpm",
        "openlogi-v99.0.0-linux-{arch}.rpm.minisig",
        "SHA256SUMS",
    ];

    /// An engine source failing the way the real manifest does on Linux,
    /// which publishes no Linux asset.
    struct NoLinuxAsset;

    impl gpui_updater::UpdateSource for NoLinuxAsset {
        fn fetch_latest(&self) -> gpui_updater::Result<gpui_updater::Release> {
            Err(gpui_updater::Error::NoMatchingAsset {
                target_os: "linux",
                target_arch: "x86_64",
            })
        }
    }

    /// A Linux path, verifying with `key`, whose check found 99.0.0
    /// publishing `names`.
    fn linux_update_available(key: Option<&'static str>, names: &[&str]) -> LinuxUpdate {
        let arch =
            openlogi_core::linux_package::arch_tag(std::env::consts::ARCH).unwrap_or("amd64");
        let release = LatestRelease {
            version: gpui_updater::Version::new(99, 0, 0),
            assets: names
                .iter()
                .map(|name| {
                    let name = name.replace("{arch}", arch);
                    ReleaseAsset {
                        browser_download_url: format!("https://example.test/{name}"),
                        name,
                    }
                })
                .collect(),
        };
        let mut update = LinuxUpdate::new(key);
        let attempt = update.begin_check().unwrap();
        assert!(update.finish_check(attempt, Ok(LinuxUpdateCheck::Available(release))));
        update
    }

    /// The Linux path after a check or install that failed with `error`.
    fn linux_update_failed(error: LinuxUpdateError) -> LinuxUpdate {
        let mut update = LinuxUpdate::new(None);
        let attempt = update.begin_check().unwrap();
        assert!(update.finish_check(attempt, Err(error)));
        update
    }

    /// The reported bug: on Linux the engine's manifest check always fails
    /// with "no release asset matched", and the hero showed that error. With
    /// the Linux path installed the real view must show the Linux path's
    /// state and action instead, whatever the engine reports.
    #[gpui::test]
    fn linux_hero_ignores_engine_error(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let updater = cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::register_builtin_themes(cx);
            let (commands, _) = tokio::sync::mpsc::unbounded_channel();
            let state = cx.new(|_| {
                AppState::new(Sources::in_memory(
                    Config::ephemeral(),
                    &AssetResolver::new(),
                    commands,
                ))
            });
            AppState::set_global(state, cx);
            cx.set_global(RPM);
            let config = gpui_updater::EngineConfig::new(gpui_updater::Version::new(0, 0, 1));
            let updater = cx.new(|cx| Updater::new(NoLinuxAsset, config, cx));
            updater.update(cx, Updater::check);
            crate::platform::updater::share(updater.clone(), cx);
            updater
        });
        cx.run_until_parked();
        let engine_error = cx.read(|cx| match updater.read(cx).status() {
            UpdateStatus::Errored(error) => error.clone(),
            other => panic!("the engine must fail without a Linux asset, not {other:?}"),
        });
        assert!(
            engine_error.contains("no release asset matched"),
            "{engine_error}"
        );

        let handle = cx.open_window(gpui::size(px(920.), px(640.)), |window, cx| {
            let settings = cx.new(|cx| SettingsView::new(SettingsPage::Updates, window, cx));
            gpui_kit::component::Root::new(settings, window, cx)
        });
        let mut visual = VisualTestContext::from_window(handle.into(), cx);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        // Without the Linux path (macOS, Windows) the engine's state is the
        // hero's: the same view does render this failure.
        assert!(visual.debug_bounds("update-pill-failed").is_some());
        assert!(visual.debug_bounds("update-check").is_some());
        assert!(visual.debug_bounds("update-install").is_none());

        cx.update(|cx| cx.set_global(linux_update_available(Some("RWQfixture"), FULL_RPM)));
        cx.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual.debug_bounds("update-pill-failed").is_none(),
            "the engine's expected failure must not reach the Linux hero"
        );
        assert!(visual.debug_bounds("update-pill-available").is_some());
        assert!(visual.debug_bounds("update-install").is_some());
        assert!(visual.debug_bounds("update-check").is_none());
        assert!(visual.debug_bounds("update-status-message").is_some());
        let hero = visual.update(|_, cx| hero_content(&updater, cx));
        assert_eq!(hero.path, HeroPath::Linux);
        assert_eq!(
            hero.message.as_deref(),
            Some("Version 99.0.0 is available.")
        );
        rust_i18n::set_locale("en");
    }

    #[test]
    fn linux_hero_offers_install_only_for_a_published_verifiable_package() {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let key = Some("RWQfixture");
        let ready = linux_hero(&linux_update_available(key, FULL_RPM), RPM);
        assert_eq!(ready.path, HeroPath::Linux);
        assert_eq!(ready.pill, Some(Pill::Available));
        assert_eq!(ready.action, HeroAction::Install);
        assert!(!ready.busy);
        assert_eq!(
            ready.message.as_deref(),
            Some("Version 99.0.0 is available.")
        );

        // The release went out without this host's package.
        let partial = linux_hero(&linux_update_available(key, &["SHA256SUMS"]), RPM);
        assert_eq!(partial.pill, Some(Pill::Available));
        assert_eq!(partial.action, HeroAction::Check);
        assert_eq!(
            partial.message.as_deref(),
            Some("Version 99.0.0 has no package for this system yet.")
        );

        // A build that cannot verify the package, and an install the app does
        // not own, both point at the package manager.
        for (update, installation) in [
            (linux_update_available(None, FULL_RPM), RPM),
            (
                linux_update_available(key, FULL_RPM),
                Installation::Detected(InstallationSource::Nix),
            ),
        ] {
            let hero = linux_hero(&update, installation);
            assert_eq!(hero.action, HeroAction::Check, "{installation:?}");
            assert_eq!(
                hero.message.as_deref(),
                Some("Version 99.0.0 is available. Update it with your package manager.")
            );
        }

        let detecting = linux_hero(
            &linux_update_available(key, FULL_RPM),
            Installation::Detecting,
        );
        assert_eq!(detecting.action, HeroAction::Check);
        assert_eq!(
            detecting.message.as_deref(),
            Some("Version 99.0.0 is available.")
        );

        let idle = linux_hero(&LinuxUpdate::new(key), RPM);
        assert_eq!(
            (idle.pill, idle.message, idle.action),
            (None, None, HeroAction::Check)
        );
    }

    #[test]
    fn linux_update_errors_follow_locale() {
        const ERRORS: [LinuxUpdateError; 6] = [
            LinuxUpdateError::Check,
            LinuxUpdateError::MissingTool("/usr/bin/pkexec"),
            LinuxUpdateError::Download,
            LinuxUpdateError::Verification,
            LinuxUpdateError::Authorization,
            LinuxUpdateError::Install,
        ];
        let message = |error| {
            linux_hero(&linux_update_failed(error), RPM)
                .message
                .unwrap()
                .to_string()
        };
        let _locale = LOCALE_LOCK.lock().unwrap();
        for (locale, check, missing_tool) in [
            (
                "en",
                "Update failed: Couldn't check GitHub Releases.",
                "Update failed: Installing updates needs /usr/bin/pkexec.",
            ),
            (
                "fr",
                "Échec de la mise à jour : Impossible de consulter GitHub Releases.",
                "Échec de la mise à jour : L’installation des mises à jour nécessite /usr/bin/pkexec.",
            ),
            (
                "zh-CN",
                "更新失败：无法检查 GitHub Releases。",
                "更新失败：安装更新需要 /usr/bin/pkexec。",
            ),
        ] {
            rust_i18n::set_locale(locale);
            assert_eq!(message(LinuxUpdateError::Check), check);
            assert_eq!(
                message(LinuxUpdateError::MissingTool("/usr/bin/pkexec")),
                missing_tool
            );
            assert_eq!(
                linux_hero(&linux_update_failed(LinuxUpdateError::Check), RPM).pill,
                Some(Pill::Failed)
            );
        }

        // Every reason has its own entry in every catalog: none falls back to
        // English, and no two reasons read alike.
        rust_i18n::set_locale("en");
        let english = ERRORS.map(linux_error_message);
        for locale in rust_i18n::available_locales!() {
            rust_i18n::set_locale(&locale);
            let reasons = ERRORS.map(linux_error_message);
            for (index, reason) in reasons.iter().enumerate() {
                assert!(!reason.contains("update_error"), "{locale}: {reason}");
                assert!(
                    !reasons[..index].contains(reason),
                    "{locale}: {reason} is not distinct"
                );
                if locale != "en" {
                    assert_ne!(reason, &english[index], "{locale} falls back to English");
                }
            }
        }
        rust_i18n::set_locale("en");
    }

    #[test]
    fn installation_labels_distinguish_sources_and_pending_from_unknown() {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        assert_eq!(installation_label(Installation::Detecting), "Detecting…");
        for (source, expected) in [
            (
                InstallationSource::Homebrew(HomebrewCask::Official),
                "Homebrew (openlogi)",
            ),
            (
                InstallationSource::Homebrew(HomebrewCask::Latest),
                "Homebrew (openlogi@latest)",
            ),
            (
                InstallationSource::LinuxPackage(LinuxPackage::Deb),
                "DEB (dpkg)",
            ),
            (InstallationSource::LinuxPackage(LinuxPackage::Rpm), "RPM"),
            (
                InstallationSource::LinuxPackage(LinuxPackage::Arch),
                "Arch Linux (pacman)",
            ),
            (InstallationSource::Nix, "Nix"),
            (InstallationSource::WindowsMsi, "Windows installer (MSI)"),
            (InstallationSource::WindowsPortable, "Portable ZIP"),
            (InstallationSource::MacAppBundle, "macOS app bundle"),
            (InstallationSource::Unknown, "Not identified"),
        ] {
            assert_eq!(installation_label(Installation::Detected(source)), expected);
        }
        rust_i18n::set_locale("zh-CN");
        assert_eq!(installation_label(Installation::Detecting), "检测中…");
        assert_eq!(
            installation_label(Installation::Detected(InstallationSource::Unknown)),
            "无法识别"
        );
        assert_eq!(
            installation_label(Installation::Detected(InstallationSource::MacAppBundle)),
            "macOS 应用包"
        );
        rust_i18n::set_locale("en");
    }

    #[gpui::test]
    fn installation_completion_refreshes_open_settings(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::register_builtin_themes(cx);
            let (commands, _) = tokio::sync::mpsc::unbounded_channel();
            let state = cx.new(|_| {
                AppState::new(Sources::in_memory(
                    Config::ephemeral(),
                    &AssetResolver::new(),
                    commands,
                ))
            });
            AppState::set_global(state, cx);
            cx.set_global(Installation::Detecting);
        });
        let mut view = None;
        let handle = cx.open_window(gpui::size(px(920.), px(640.)), |window, cx| {
            let settings = cx.new(|cx| SettingsView::new(SettingsPage::Updates, window, cx));
            view = Some(settings.clone());
            gpui_kit::component::Root::new(settings, window, cx)
        });
        let view = view.unwrap();
        let mut visual = VisualTestContext::from_window(handle.into(), cx);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        scroll_to_sources(&mut visual);
        assert_source_geometry(&mut visual);
        assert_release_link_activation(&mut visual);
        let notified = Rc::new(Cell::new(false));
        let _observer = visual.update(|_, cx| {
            let notified = notified.clone();
            cx.observe(&view, move |_, _| notified.set(true))
        });
        cx.run_until_parked();

        for source in [
            InstallationSource::Homebrew(HomebrewCask::Latest),
            InstallationSource::Unknown,
        ] {
            notified.set(false);
            cx.update(|cx| cx.set_global(Installation::Detected(source)));
            cx.run_until_parked();
            assert!(
                notified.get(),
                "the open Settings view must observe detection completion"
            );
            visual.update(|window, cx| window.draw(cx).clear(cx));
            scroll_to_sources(&mut visual);
            assert_source_geometry(&mut visual);
        }

        for (locale, scale) in [("zh-CN", UiScale::Normal), ("de", UiScale::ExtraLarge)] {
            cx.update(|cx| cx.set_global(Installation::Detected(InstallationSource::WindowsMsi)));
            cx.run_until_parked();
            notified.set(false);
            cx.update(|cx| {
                AppState::apply(cx, |state| state.commit_ui_scale(scale));
                AppState::apply(cx, |state| state.commit_language(Some(locale.into())));
            });
            cx.run_until_parked();
            assert!(
                notified.get(),
                "a live locale change must refresh installation metadata"
            );
            visual.update(|window, cx| window.draw(cx).clear(cx));
            scroll_to_sources(&mut visual);
            assert_source_geometry(&mut visual);
        }
        rust_i18n::set_locale("en");
    }

    /// Pointer clicks and Enter or Space on a focused button each pick their
    /// channel. Clicking the channel the settings already follow records it.
    #[gpui::test]
    fn channel_buttons_commit_by_pointer_and_keyboard(cx: &mut TestAppContext) {
        struct Harness;
        impl Render for Harness {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div().size_full().child(channel_segment(cx))
            }
        }
        fn picked(visual: &mut VisualTestContext) -> Option<UpdateChannel> {
            visual.update(|_, cx| {
                AppState::try_read(cx)
                    .unwrap()
                    .app_settings()
                    .update_channel
            })
        }
        fn press(visual: &mut VisualTestContext, key: &'static str) {
            let keystroke = Keystroke::parse(key).unwrap();
            visual.simulate_event(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            visual.simulate_event(KeyUpEvent { keystroke });
        }

        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::register_builtin_themes(cx);
            let (commands, _) = tokio::sync::mpsc::unbounded_channel();
            let state = cx.new(|_| {
                AppState::new(Sources::in_memory(
                    Config::ephemeral(),
                    &AssetResolver::new(),
                    commands,
                ))
            });
            AppState::set_global(state, cx);
        });
        let handle = cx.open_window(gpui::size(px(400.), px(120.)), |window, cx| {
            let harness = cx.new(|_| Harness);
            gpui_kit::component::Root::new(harness, window, cx)
        });
        let mut visual = VisualTestContext::from_window(handle.into(), cx);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(picked(&mut visual), None);

        let followed = crate::platform::updater::followed_channel(None);
        let id = match followed {
            UpdateChannel::Stable => "update-channel-stable",
            UpdateChannel::Beta => "update-channel-beta",
        };
        let button = visual.debug_bounds(id).unwrap();
        visual.simulate_click(button.center(), Modifiers::default());
        assert_eq!(picked(&mut visual), Some(followed));

        // The harness has two tab stops, in `UpdateChannel::ALL` order.
        for (key, channel) in [
            ("enter", UpdateChannel::Beta),
            ("space", UpdateChannel::Stable),
            ("space", UpdateChannel::Beta),
            ("enter", UpdateChannel::Stable),
        ] {
            let stops = UpdateChannel::ALL
                .iter()
                .position(|candidate| *candidate == channel)
                .unwrap()
                + 1;
            visual.update(|window, cx| {
                window.blur(cx);
                for _ in 0..stops {
                    window.focus_next(cx);
                }
            });
            press(&mut visual, key);
            assert_eq!(picked(&mut visual), Some(channel), "{key} on {channel:?}");
        }
        rust_i18n::set_locale("en");
    }

    /// Scroll the page to its end, where the source group is, and redraw.
    ///
    /// The second scroll works around a `SettingPage` limitation, not a test
    /// framework one. `SettingPage` keeps the virtualized list state of a page
    /// and resets it only when the page's group count changes, so after a
    /// scale change the first scroll still uses the group heights measured at
    /// the old scale and can stop short of the end. The redraw remeasures the
    /// groups it draws, and the second scroll uses those heights.
    fn scroll_to_sources(visual: &mut VisualTestContext) {
        for _ in 0..2 {
            visual.simulate_event(ScrollEvent {
                position: point(px(700.), px(500.)),
                delta: ScrollDelta::Pixels(point(px(0.), px(-1000.))),
                ..Default::default()
            });
            visual.update(|window, cx| window.draw(cx).clear(cx));
        }
    }

    fn assert_source_geometry(visual: &mut VisualTestContext) {
        let value = visual.debug_bounds("installation-source-value").unwrap();
        let link = visual.debug_bounds("update-source-link").unwrap();
        let policy = visual.debug_bounds("update-connection-policy").unwrap();
        assert_eq!(value.right(), policy.right());
        assert_eq!(link.right(), value.right());
        assert!(value.left() > policy.left());
        assert!(link.left() > policy.left());
        assert!(link.top() >= value.bottom());
        assert!(value.top() >= px(0.) && value.bottom() <= px(640.));
        assert!(policy.right() <= px(920.));
        assert!(policy.bottom() <= px(640.));
        visual.update(|window, cx| {
            // Source fields are primary row text, not captions. Native
            // SettingItem owns both labels, including their size and weight.
            let expected_size = Some(gpui::rems(0.875).into());
            assert_eq!(installation_value(cx).style().text.font_size, expected_size);
            assert_eq!(release_link(cx).style().text.font_size, expected_size);
            let scale = window.scale_factor();
            let quads = window.painted_quads();
            let card = quads
                .iter()
                .rev()
                .find(|quad| {
                    quad.background == cx.theme().tokens.group_box.into()
                        && quad.bounds.contains(&value.center().scale(scale))
                })
                .expect("the installation row must have a painted card surface");
            assert!(card.bounds.contains(&link.center().scale(scale)));
            assert_eq!(
                policy.left().scale(scale),
                card.bounds.left() + window.rem_size().scale(scale),
                "the footer must align with the standard setting labels' content inset"
            );
            assert!(
                policy.top().scale(scale) > card.bounds.bottom(),
                "the policy must be below the painted card, not a row within its fill"
            );
        });
    }

    fn assert_release_link_activation(visual: &mut VisualTestContext) {
        let link = visual.debug_bounds("update-source-link").unwrap();
        assert_eq!(visual.opened_url(), None);
        visual.simulate_click(link.center(), Modifiers::default());
        assert_eq!(visual.opened_url().as_deref(), Some(RELEASES_URL));
        visual.update(|window, cx| window.draw(cx).clear(cx));

        for key in ["enter", "space"] {
            // Clear the preceding outcome so a missing keyboard handler fails.
            visual.update(|_, cx| cx.open_url("about:blank"));
            let keystroke = Keystroke::parse(key).unwrap();
            visual.simulate_event(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            visual.simulate_event(KeyUpEvent { keystroke });
            assert_eq!(visual.opened_url().as_deref(), Some(RELEASES_URL));
        }
    }
}
