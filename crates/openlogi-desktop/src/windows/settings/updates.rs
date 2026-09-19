//! Updates settings page.

use super::{
    App, AppState, Button, Disableable, Entity, FontWeight, IconName, InteractiveElement as _,
    ParentElement, RELEASES_URL, SettingField, SettingGroup, SettingItem, SettingPage, Sizable,
    StatefulInteractiveElement as _, Styled, Tag, UpdateStatus, Updater, div, h_flex, img, px,
    v_flex,
};
use crate::platform::installation::{HomebrewCask, Installation, InstallationSource, LinuxPackage};
use crate::ui::theme::Typography as _;
use gpui_base::Link;
use gpui_component::ActiveTheme as _;

/// The Updates page: a hero card with the running build, its update status, and
/// the contextual check / install / restart action; the opt-in auto-check and
/// auto-install switches; installation ownership and where updates come from.
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
                    |enabled, cx| {
                        AppState::apply(cx, |state| state.commit_auto_install_updates(enabled));
                    },
                ),
            )
            .description(tr!("updates.automatic_update_description")),
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
            div()
                .text_caption()
                .text_color(crate::ui::theme::palette(cx).text_muted)
                .debug_selector(|| "update-connection-policy".into())
                .child(tr!("updates.update_connection_policy"))
        });
    SettingPage::new(tr!("updates.updates"))
        .icon(IconName::ArrowDown)
        .resettable(false)
        .description(tr!("updates.update_network_privacy_description"))
        .group(hero)
        .group(toggles)
        .group(source)
}

/// The Updates hero row: logo, name + version, a status pill, the live status
/// message (or channel), and the one contextual action button.
fn update_hero(updater: &Entity<Updater>, cx: &mut App) -> gpui::Div {
    let pal = crate::ui::theme::palette(cx);
    let status = updater.read(cx).status().clone();

    // A short status tag for the settled states (semantic colours from the theme);
    // transient states carry their detail in the message line instead.
    let pill = match &status {
        UpdateStatus::UpToDate => Some(Tag::success().child(tr!("updates.up_to_date"))),
        UpdateStatus::Available(_) => Some(Tag::info().child(tr!("updates.update_available"))),
        UpdateStatus::Staged(_) => Some(Tag::success().child(tr!("updates.update_ready"))),
        UpdateStatus::Errored(_) => Some(Tag::danger().child(tr!("updates.update_failed"))),
        _ => None,
    };

    let message = match &status {
        UpdateStatus::Idle | UpdateStatus::UpToDate => None,
        UpdateStatus::Checking => Some(tr!("updates.checking_for_updates")),
        UpdateStatus::Available(v) => Some(tr!("updates.update_version_available", version => v)),
        UpdateStatus::Downloading { downloaded, total } => Some(match total {
            Some(t) if *t > 0 => {
                tr!("updates.update_downloading_percent", percent => (*downloaded * 100 / *t).to_string())
            }
            _ => {
                tr!("updates.update_downloading_size", size => (*downloaded / 1_048_576).to_string())
            }
        }),
        UpdateStatus::Installing => Some(tr!("updates.installing")),
        UpdateStatus::Staged(v) => Some(tr!("updates.update_version_ready", version => v)),
        UpdateStatus::Errored(e) => Some(tr!("updates.update_failed_message", error => e.clone())),
    };

    let busy = matches!(
        status,
        UpdateStatus::Checking | UpdateStatus::Downloading { .. } | UpdateStatus::Installing
    );

    let action = {
        let u = updater.clone();
        match &status {
            UpdateStatus::Available(_) => Button::new("update-install")
                .outline()
                .label(tr!("updates.download_install"))
                .on_click(move |_, _, cx| {
                    u.update(cx, Updater::download_and_install);
                }),
            UpdateStatus::Staged(_) => Button::new("update-restart")
                .outline()
                .label(tr!("updates.restart_to_update"))
                .on_click(move |_, _, cx| {
                    u.update(cx, |u, cx| u.restart(cx));
                }),
            _ => Button::new("update-check")
                .outline()
                .label(tr!("updates.check_for_updates_action"))
                .on_click(move |_, _, cx| {
                    u.update(cx, Updater::check);
                }),
        }
    };

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
                                .children(pill.map(|tag| tag.small().rounded_full())),
                        )
                        .child(
                            div()
                                .text_caption()
                                .text_color(pal.text_muted)
                                .truncate()
                                .child(message.unwrap_or_else(|| tr!("updates.stable_channel"))),
                        ),
                ),
        )
        .child(div().flex_shrink_0().child(action.disabled(busy)))
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
    let focus_color = cx.theme().ring;
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
        .hover(move |style| style.bg(pal.control_hover))
        .active(move |style| style.bg(pal.control))
        .focus_visible(move |style| style.bg(focus_color).text_color(pal.page))
        .debug_selector(|| "update-source-link".into())
        .child(div().underline().child("GitHub Releases"))
        .child(gpui_component::Icon::new(IconName::ExternalLink).size_3p5())
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui::{
        AppContext as _, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, ScrollDelta,
        ScrollWheelEvent, TestAppContext, VisualTestContext, point,
    };
    use openlogi_core::config::{Config, UiScale};

    use super::*;
    use crate::services::{assets::AssetResolver, i18n::LOCALE_LOCK};
    use crate::state::Sources;
    use crate::windows::settings::{SettingsPage, SettingsView};

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
            gpui_component::init(cx);
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
            gpui_component::Root::new(settings, window, cx)
        });
        let view = view.unwrap();
        let mut visual = VisualTestContext::from_window(handle.into(), cx);
        visual.update(|window, cx| window.draw(cx).clear(cx));
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
            visual.simulate_event(ScrollWheelEvent {
                position: point(px(700.), px(500.)),
                delta: ScrollDelta::Pixels(point(px(0.), px(-1000.))),
                ..Default::default()
            });
            visual.update(|window, cx| window.draw(cx).clear(cx));
            assert_source_geometry(&mut visual);
        }
        rust_i18n::set_locale("en");
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
