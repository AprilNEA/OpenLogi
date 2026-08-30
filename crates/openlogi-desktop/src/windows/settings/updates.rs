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
                tr!("Check for updates"),
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
            .description(tr!(
                "Check once per launch for a new version (query only — no automatic download)."
            )),
        )
        .item(
            SettingItem::new(
                tr!("Automatically download and install"),
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
            .description(tr!(
                "Download updates in the background and apply them the next time OpenLogi restarts."
            )),
        );

    let source = SettingGroup::new().item(SettingItem::render(move |_, _, cx| update_source(cx)));
    SettingPage::new(tr!("Updates"))
        .icon(IconName::ArrowDown)
        .resettable(false)
        .description(tr!(
            "Off by default — checking for updates is OpenLogi's only optional outbound network request."
        ))
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
        UpdateStatus::UpToDate => Some(Tag::success().child(tr!("Up to date"))),
        UpdateStatus::Available(_) => Some(Tag::info().child(tr!("Update available"))),
        UpdateStatus::Staged(_) => Some(Tag::success().child(tr!("Update ready"))),
        UpdateStatus::Errored(_) => Some(Tag::danger().child(tr!("Update failed"))),
        _ => None,
    };

    let message = match &status {
        UpdateStatus::Idle | UpdateStatus::UpToDate => None,
        UpdateStatus::Checking => Some(tr!("Checking for updates…")),
        UpdateStatus::Available(v) => Some(tr!("Version %{version} is available.", version => v)),
        UpdateStatus::Downloading { downloaded, total } => Some(match total {
            Some(t) if *t > 0 => {
                tr!("Downloading… %{percent}%", percent => (*downloaded * 100 / *t).to_string())
            }
            _ => tr!("Downloading… %{size} MB", size => (*downloaded / 1_048_576).to_string()),
        }),
        UpdateStatus::Installing => Some(tr!("Installing…")),
        UpdateStatus::Staged(v) => Some(tr!("Version %{version} is ready.", version => v)),
        UpdateStatus::Errored(e) => Some(tr!("Update failed: %{error}", error => e.clone())),
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
                .label(tr!("Download & Install"))
                .on_click(move |_, _, cx| {
                    u.update(cx, Updater::download_and_install);
                }),
            UpdateStatus::Staged(_) => Button::new("update-restart")
                .outline()
                .label(tr!("Restart to Update"))
                .on_click(move |_, _, cx| {
                    u.update(cx, |u, cx| u.restart(cx));
                }),
            _ => Button::new("update-check")
                .outline()
                .label(tr!("Check for Updates"))
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
                .child(
                    div()
                        .size(px(52.))
                        .flex_none()
                        .child(img(crate::app_assets::LOGO).size(px(52.))),
                )
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
                                .child(message.unwrap_or_else(|| tr!("Stable channel"))),
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
                                .child(tr!("Update source")),
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
                            .label(tr!("View changelog"))
                            .on_click(|_, _, cx| cx.open_url(RELEASES_URL)),
                    ),
                ),
        )
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!(
                    "No background updater — OpenLogi only connects when you turn on automatic checks or click Check for Updates."
                )),
        )
}
