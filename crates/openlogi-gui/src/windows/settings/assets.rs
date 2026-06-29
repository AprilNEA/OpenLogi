//! Assets (device-image cache) settings page.

use super::{
    App, AppState, AssetCommand, AssetControl, BorrowAppContext, IconName, InteractiveElement,
    IntoElement, Palette, ParentElement, SettingField, SettingGroup, SettingItem, SettingPage,
    SharedString, StatefulInteractiveElement, Styled, div,
};

pub(super) fn assets_page(pal: Palette, cache_desc: SharedString) -> SettingPage {
    let group = SettingGroup::new()
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
                "Fetch device renders from assets.openlogi.org when a device connects. When off, OpenLogi makes no asset network requests; bundled art and the silhouette still show."
            )),
        )
        .item(
            SettingItem::new(
                tr!("Refresh assets"),
                SettingField::render(move |_, _, _| {
                    action_button("assets-refresh", tr!("Refresh"), pal, |cx| {
                        send_asset_command(cx, AssetCommand::Refresh);
                    })
                }),
            )
            .description(tr!("Re-download images for the connected devices now.")),
        )
        .item(
            SettingItem::new(
                tr!("Clear cache"),
                SettingField::render(move |_, _, _| {
                    action_button("assets-clear", tr!("Clear"), pal, |cx| {
                        send_asset_command(cx, AssetCommand::ClearCache);
                        cx.refresh_windows();
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
        .rounded_md()
        .border_1()
        .border_color(pal.border)
        .text_xs()
        .cursor_pointer()
        .hover(move |s| s.bg(pal.surface_hover))
        .child(label)
        .on_click(move |_, _, cx| on_click(cx))
}

/// Push a manual asset action to the main loop's [`AssetControl`] channel.
fn send_asset_command(cx: &App, cmd: AssetCommand) {
    if let Some(ctrl) = cx.try_global::<AssetControl>() {
        let _ = ctrl.0.send(cmd);
    }
}
