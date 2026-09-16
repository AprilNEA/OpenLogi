//! Updates settings page.

use super::{
    App, AppState, Button, ButtonVariants, Disableable, Entity, FontWeight, IconName,
    ParentElement, RELEASES_URL, SettingField, SettingGroup, SettingItem, SettingPage, Sizable,
    StateEvent, Styled, Tag, UpdateStatus, Updater, div, h_flex, img, px, v_flex,
};
use crate::ui::theme::Typography as _;

/// The Updates page: a hero card with the running build, its update status, and
/// the contextual check / install / restart action; the opt-in auto-check and
/// auto-install switches; and where updates come from.
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
                        AppState::update(cx, move |state, cx| {
                            state.set_check_for_updates(enabled);
                            cx.emit(StateEvent::SettingsChanged);
                        });
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
                        AppState::update(cx, move |state, cx| {
                            state.set_auto_install_updates(enabled);
                            cx.emit(StateEvent::SettingsChanged);
                        });
                    },
                ),
            )
            .description(tr!("updates.automatic_update_description")),
        );

    let source = SettingGroup::new().item(SettingItem::render(move |_, _, cx| update_source(cx)));
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

/// The "where updates come from" row plus the privacy footnote.
fn update_source(cx: &App) -> gpui::Div {
    let pal = crate::ui::theme::palette(cx);
    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    // Shrink-safe like the hero row above: the text yields,
                    // the button stays whole.
                    v_flex()
                        .gap_1()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .child(tr!("updates.update_source")),
                        )
                        .child(
                            div()
                                .text_caption()
                                .text_color(pal.text_muted)
                                .truncate()
                                .child("github.com/AprilNEA/OpenLogi/releases"),
                        ),
                )
                .child(
                    div().flex_shrink_0().child(
                        Button::new("update-changelog")
                            .ghost()
                            .icon(IconName::ExternalLink)
                            .label(tr!("updates.view_changelog"))
                            .on_click(|_, _, cx| cx.open_url(RELEASES_URL)),
                    ),
                ),
        )
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("updates.update_connection_policy")),
        )
}
