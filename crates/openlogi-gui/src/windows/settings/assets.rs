//! Assets (device-image cache) settings page.

use std::time::Duration;

use super::{
    App, AppState, AssetCommand, AssetControl, AssetSourcePreference, BorrowAppContext, Entity,
    IconName, IndexPath, InteractiveElement, IntoElement, Palette, ParentElement, Select,
    SelectItem, SelectState, SettingField, SettingGroup, SettingItem, SettingPage, SettingsView,
    SharedString, Sizable, StatefulInteractiveElement, Styled, div, px,
};

#[derive(Clone)]
pub(super) struct AssetSourceOption {
    source: AssetSourcePreference,
}

impl SelectItem for AssetSourceOption {
    type Value = AssetSourcePreference;

    fn title(&self) -> SharedString {
        match self.source {
            AssetSourcePreference::Automatic => tr!("Automatic (recommended)"),
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
    pal: Palette,
    cache_desc: SharedString,
) -> SettingPage {
    let refresh_view = view.clone();
    let group = SettingGroup::new()
        .item(
            SettingItem::new(
                tr!("Asset source"),
                SettingField::render(move |_, _, _| {
                    asset_source_select_field(asset_source_select.clone())
                }),
            )
            .description(tr!(
                "Automatic uses the first healthy mirror. Choose a source to pin downloads; OPENLOGI_ASSETS still takes precedence."
            )),
        )
        .item(
            SettingItem::new(
                tr!("Automatically download device images"),
                SettingField::switch(
                    |cx| {
                        cx.try_global::<AppState>()
                            .is_none_or(|s| s.app_settings().auto_download_assets)
                    },
                    |enabled, cx| {
                        cx.update_global::<AppState, _>(move |s, _| {
                            s.set_auto_download_assets(enabled);
                        });
                        // Re-enabling should fetch right away, not wait for the
                        // next device event.
                        if enabled {
                            send_asset_command(cx, AssetCommand::Refresh);
                        }
                        cx.refresh_windows();
                    },
                ),
            )
            .description(tr!(
                "Fetch device renders from the selected source when a device connects. When off, OpenLogi makes no asset network requests; bundled art and the silhouette still show."
            )),
        )
        .item(
            SettingItem::new(
                tr!("Refresh assets"),
                SettingField::render(move |_, _, _| {
                    let view = refresh_view.clone();
                    action_button("assets-refresh", tr!("Refresh"), pal, move |cx| {
                        send_asset_command(cx, AssetCommand::Refresh);
                        // Give the spawned sync a moment to land small fetches,
                        // then re-quote the size row so the click visibly did
                        // something. Best-effort — a longer sync is caught by
                        // the next action or window reopen.
                        refresh_cache_desc_after(&view, Duration::from_secs(2), cx);
                    })
                }),
            )
            .description(tr!("Re-download images for the connected devices now.")),
        )
        .item(
            SettingItem::new(
                tr!("Clear cache"),
                SettingField::render(move |_, _, _| {
                    let view = view.clone();
                    action_button("assets-clear", tr!("Clear"), pal, move |cx| {
                        send_asset_command(cx, AssetCommand::ClearCache);
                        cx.refresh_windows();
                        // The wipe runs on the main loop's channel arm, not
                        // synchronously here — without a recompute the row
                        // keeps quoting the pre-Clear size until the window
                        // reopens, which reads as the button doing nothing.
                        refresh_cache_desc_after(&view, Duration::from_millis(750), cx);
                    })
                }),
            )
            .description(cache_desc),
        )
        .item(
            SettingItem::new(
                tr!("Cache location"),
                SettingField::render(move |_, _, _| {
                    action_button("assets-open", tr!("Open"), pal, |_| {
                        crate::asset::reveal_cache_in_file_manager();
                    })
                }),
            )
            .description(tr!("Show the downloaded-images folder in your file manager.")),
        );

    SettingPage::new(tr!("Assets"))
        .icon(IconName::HardDrive)
        .resettable(false)
        .group(group)
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "built inside an `Fn` render closure, so a `&Entity` parameter would make \
              the returned element borrow a captured variable; `Entity` is a cheap handle"
)]
fn asset_source_select_field(
    asset_source_select: Entity<SelectState<Vec<AssetSourceOption>>>,
) -> impl IntoElement {
    div().flex_shrink_0().w(px(220.)).h_6().child(
        Select::new(&asset_source_select)
            .small()
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
    #[allow(
        clippy::cast_precision_loss,
        reason = "the cache is at most a few hundred MB; f64 is exact far past that, \
                  and this is a display-only size"
    )]
    let mb = crate::asset::cache_size_bytes() as f64 / 1024.0 / 1024.0;
    tr!("Downloaded images currently use %{size}.", size => format!("{mb:.1} MB"))
}

/// A small bordered text button matching the permission rows' "Open" control.
fn action_button(
    id: &'static str,
    label: SharedString,
    pal: Palette,
    on_click: impl Fn(&mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex_shrink_0()
        .px_2()
        .py_1()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        .text_xs()
        .cursor_pointer()
        .hover(move |s| s.bg(pal.surface_hover))
        .child(label)
        .on_click(move |_, _, cx| on_click(cx))
}

/// Push a manual asset action to the main loop's [`AssetControl`] channel.
pub(super) fn send_asset_command(cx: &App, cmd: AssetCommand) {
    if let Some(ctrl) = cx.try_global::<AssetControl>() {
        let _ = ctrl.0.send(cmd);
    }
}
