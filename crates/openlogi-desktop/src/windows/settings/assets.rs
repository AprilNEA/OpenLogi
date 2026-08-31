//! Assets (device-image cache) settings page.

use crate::ui::theme::Typography as _;
use gpui_base::Button as BaseButton;
use std::time::Duration;

use crate::ui::components::control_select;

use super::{
    App, AppState, AssetCommand, AssetControl, AssetSourcePreference, Entity, IconName, IndexPath,
    InteractiveElement, IntoElement, Palette, ParentElement, SelectItem, SelectState, SettingField,
    SettingGroup, SettingItem, SettingPage, SettingsView, SharedString, StateEvent, Styled, div,
    px,
};

#[derive(Clone)]
pub(super) struct AssetSourceOption {
    source: AssetSourcePreference,
}

impl SelectItem for AssetSourceOption {
    type Value = AssetSourcePreference;

    fn title(&self) -> SharedString {
        match self.source {
            AssetSourcePreference::Automatic => {
                gpui::SharedString::from(rust_i18n::t!("assets.automatic_recommended"))
            }
            AssetSourcePreference::OpenLogi => SharedString::from("OpenLogi"),
            AssetSourcePreference::Cloudflare => SharedString::from("Cloudflare"),
            AssetSourcePreference::Fastly => SharedString::from("Fastly"),
        }
    }

    fn value(&self) -> &Self::Value {
        &self.source
    }
}

pub(super) fn asset_source_options() -> Vec<AssetSourceOption> {
    [
        AssetSourcePreference::Automatic,
        AssetSourcePreference::OpenLogi,
        AssetSourcePreference::Cloudflare,
        AssetSourcePreference::Fastly,
    ]
    .into_iter()
    .map(|source| AssetSourceOption { source })
    .collect()
}

pub(super) fn selected_source_index(
    current: AssetSourcePreference,
    options: &[AssetSourceOption],
) -> IndexPath {
    let row = options
        .iter()
        .position(|option| option.source == current)
        .unwrap_or_default();
    IndexPath::default().row(row)
}

pub(super) fn assets_page(
    view: Entity<SettingsView>,
    asset_source_select: Entity<SelectState<Vec<AssetSourceOption>>>,
    cache_desc: SharedString,
) -> SettingPage {
    let refresh_view = view.clone();
    let group = SettingGroup::new()
        .item(
            SettingItem::new(
                rust_i18n::t!("assets.asset_source"),
                SettingField::render(move |_, _, _| {
                    asset_source_select_field(asset_source_select.clone())
                }),
            )
            .description(gpui::SharedString::from(rust_i18n::t!(
                "assets.asset_source_description"
            ))),
        )
        .item(
            SettingItem::new(
                rust_i18n::t!("assets.automatically_download_device_images"),
                SettingField::switch(
                    |cx| {
                        AppState::try_read(cx).is_none_or(|s| s.app_settings().auto_download_assets)
                    },
                    |enabled, cx| {
                        AppState::update(cx, move |state, cx| {
                            state.set_auto_download_assets(enabled);
                            cx.emit(StateEvent::SettingsChanged);
                        });
                        // Re-enabling should fetch right away, not wait for the
                        // next device event.
                        if enabled {
                            send_asset_command(cx, AssetCommand::Refresh);
                        }
                    },
                ),
            )
            .description(gpui::SharedString::from(rust_i18n::t!(
                "assets.automatic_device_images_description"
            ))),
        )
        .item(
            SettingItem::new(
                rust_i18n::t!("assets.refresh_assets"),
                SettingField::render(move |_, _, cx| {
                    let view = refresh_view.clone();
                    let pal = crate::ui::theme::palette(cx);
                    action_button(
                        "assets-refresh",
                        gpui::SharedString::from(rust_i18n::t!("common.refresh")),
                        pal,
                        move |cx| {
                            send_asset_command(cx, AssetCommand::Refresh);
                            // Give the spawned sync a moment to land small fetches,
                            // then re-quote the size row so the click visibly did
                            // something. Best-effort — a longer sync is caught by
                            // the next action or window reopen.
                            refresh_cache_desc_after(&view, Duration::from_secs(2), cx);
                        },
                    )
                }),
            )
            .description(gpui::SharedString::from(rust_i18n::t!(
                "assets.refresh_device_images_description"
            ))),
        )
        .item(
            SettingItem::new(
                rust_i18n::t!("assets.clear_cache"),
                SettingField::render(move |_, _, cx| {
                    let view = view.clone();
                    let pal = crate::ui::theme::palette(cx);
                    action_button(
                        "assets-clear",
                        gpui::SharedString::from(rust_i18n::t!("common.clear")),
                        pal,
                        move |cx| {
                            send_asset_command(cx, AssetCommand::ClearCache);
                            // The wipe runs on the main loop's channel arm, not
                            // synchronously here — without a recompute the row
                            // keeps quoting the pre-Clear size until the window
                            // reopens, which reads as the button doing nothing.
                            refresh_cache_desc_after(&view, Duration::from_millis(750), cx);
                        },
                    )
                }),
            )
            .description(cache_desc),
        )
        .item(
            SettingItem::new(
                rust_i18n::t!("assets.cache_location"),
                SettingField::render(move |_, _, cx| {
                    let pal = crate::ui::theme::palette(cx);
                    action_button(
                        "assets-open",
                        gpui::SharedString::from(rust_i18n::t!("common.open")),
                        pal,
                        |_| {
                            crate::services::assets::reveal_cache_in_file_manager();
                        },
                    )
                }),
            )
            .description(gpui::SharedString::from(rust_i18n::t!(
                "assets.show_downloaded_images_description"
            ))),
        );

    SettingPage::new(rust_i18n::t!("assets.assets"))
        .icon(IconName::HardDrive)
        .resettable(false)
        .group(group)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "built inside an `Fn` render closure, so a `&Entity` parameter would make \
              the returned element borrow a captured variable; `Entity` is a cheap handle"
)]
fn asset_source_select_field(
    asset_source_select: Entity<SelectState<Vec<AssetSourceOption>>>,
) -> impl IntoElement {
    div().flex_shrink_0().w(px(220.)).h_6().child(
        control_select(&asset_source_select)
            .w(px(220.))
            .menu_width(px(220.)),
    )
}

/// Re-walk the cache and swap the size blurb into the view after `delay`. The
/// manual actions run on the main loop's channel arm, not synchronously in the
/// click handler, so an immediate recompute would race the wipe/fetch.
fn refresh_cache_desc_after(view: &Entity<SettingsView>, delay: Duration, cx: &mut App) {
    // Weak: the window can close before the timer fires; a strong handle
    // would keep the dead view alive just to update it.
    let view = view.downgrade();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(delay).await;
        view.update(cx, |this, cx| {
            this.asset_cache_desc = cache_size_description();
            cx.notify();
        })
        .ok();
    })
    .detach();
}

/// Human-readable size of the on-disk asset cache, for the "Clear cache" row.
/// Computed once when the Settings window opens (`asset_cache_desc`), not per
/// render.
pub(super) fn cache_size_description() -> SharedString {
    #[expect(
        clippy::cast_precision_loss,
        reason = "the cache is at most a few hundred MB; f64 is exact far past that, \
                  and this is a display-only size"
    )]
    let mb = crate::services::assets::cache_size_bytes() as f64 / 1024.0 / 1024.0;
    gpui::SharedString::from(
        rust_i18n::t!("assets.downloaded_images_size", size => format!("{mb:.1} MB")),
    )
}

/// A small bordered text button matching the permission rows' "Open" control.
fn action_button(
    id: &'static str,
    label: SharedString,
    pal: Palette,
    on_click: impl Fn(&mut App) + 'static,
) -> impl IntoElement {
    BaseButton::new(id)
        .accessibility_label(label.clone())
        .flex_shrink_0()
        .px_2()
        .py_1()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        .text_caption()
        .cursor_pointer()
        .bg(pal.control)
        .hover(move |s| s.bg(pal.control_hover))
        .focus_visible(move |s| s.bg(pal.control_hover))
        .child(label)
        .on_click(move |_, _, cx| on_click(cx))
}

/// Push a manual asset action to the main loop's [`AssetControl`] channel.
pub(super) fn send_asset_command(cx: &App, cmd: AssetCommand) {
    if let Some(ctrl) = cx.try_global::<AssetControl>() {
        let _ = ctrl.0.send(cmd);
    }
}
