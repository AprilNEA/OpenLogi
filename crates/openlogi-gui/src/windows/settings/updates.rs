//! Updates settings page.

use super::{
    AnyElement, App, AppState, BorrowAppContext, Button, ButtonVariants, Disableable, Entity,
    FontWeight, IconName, IntoElement, Palette, ParentElement, RELEASES_URL, SettingField,
    SettingGroup, SettingItem, SettingPage, Sizable, Styled, Tag, UpdateStatus, Updater, div,
    h_flex, img, px, v_flex,
};

/// The Updates page: a hero card with the running build, its update status, and
/// the contextual check / install / restart action; the opt-in auto-check and
/// auto-install switches; and where updates come from.
pub(super) fn updates_page(updater: Entity<Updater>, pal: Palette) -> SettingPage {
    let hero = SettingGroup::new().item(SettingItem::render(move |_, _, cx| {
        update_hero(&updater, pal, cx)
    }));

    let toggles = SettingGroup::new().item(
        SettingItem::new(
            tr!("Check for updates"),
            SettingField::switch(
                |cx| {
                    cx.try_global::<AppState>()
                        .is_some_and(|s| s.app_settings().check_for_updates)
                },
                |enabled, cx| {
                    cx.update_global::<AppState, _>(move |s, _| {
                        s.set_check_for_updates(enabled);
                    });
                    cx.refresh_windows();
                },
            ),
        )
        .description(tr!(
            "Check once per launch for a new version (query only — no automatic download)."
        )),
    );
    // Offering the auto-install switch on a platform whose install flow isn't
    // wired (Windows, today) would be a control that silently does nothing —
    // hide it instead; checks there are notify-only.
    let toggles = if crate::platform::updater::INSTALL_SUPPORTED {
        toggles.item(
            SettingItem::new(
                tr!("Automatically download and install"),
                SettingField::switch(
                    |cx| {
                        cx.try_global::<AppState>()
                            .is_some_and(|s| s.app_settings().auto_install_updates)
                    },
                    |enabled, cx| {
                        cx.update_global::<AppState, _>(move |s, _| {
                            s.set_auto_install_updates(enabled);
                        });
                        cx.refresh_windows();
                    },
                ),
            )
            .description(tr!(
                "Download updates in the background and apply them the next time OpenLogi restarts."
            )),
        )
    } else {
        toggles
    };

    let source = SettingGroup::new().item(SettingItem::render(move |_, _, _| update_source(pal)));

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
fn update_hero(updater: &Entity<Updater>, pal: Palette, cx: &mut App) -> AnyElement {
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
            // No wired install flow (Windows): a found update routes to the
            // GitHub release for a manual download instead of feeding
            // gpui-updater an artifact its installer can't apply.
            UpdateStatus::Available(_) if !crate::platform::updater::INSTALL_SUPPORTED => {
                Button::new("update-download")
                    .outline()
                    .label(tr!("Download from GitHub"))
                    .on_click(|_, _, cx| cx.open_url(RELEASES_URL))
            }
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
            h_flex()
                .items_center()
                .gap_3()
                .child(img(crate::app_assets::LOGO).w(px(52.)).h(px(52.)))
                .child(
                    v_flex()
                        .gap_1()
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
                                .text_xs()
                                .text_color(pal.text_muted)
                                .child(message.unwrap_or_else(|| tr!("Stable channel"))),
                        ),
                ),
        )
        .child(action.disabled(busy))
        .into_any_element()
}

/// The "where updates come from" row plus the privacy footnote.
fn update_source(pal: Palette) -> AnyElement {
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
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .child(tr!("Update source")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(pal.text_muted)
                                .child("github.com/AprilNEA/OpenLogi/releases"),
                        ),
                )
                .child(
                    Button::new("update-changelog")
                        .ghost()
                        .icon(IconName::ExternalLink)
                        .label(tr!("View changelog"))
                        .on_click(|_, _, cx| cx.open_url(RELEASES_URL)),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(pal.text_muted)
                .child(tr!(
                    "No background updater — OpenLogi only connects when you turn on automatic checks or click Check for Updates."
                )),
        )
        .into_any_element()
}
