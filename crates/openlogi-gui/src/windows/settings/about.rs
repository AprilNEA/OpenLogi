//! About settings page.

use super::{
    AnyElement, App, Button, ButtonVariants, ClipboardItem, Entity, FontWeight, HELP_URL, Icon,
    IconName, IntoElement, Palette, ParentElement, RELEASES_URL, REPO_URL, SettingGroup,
    SettingItem, SettingPage, SettingsView, SharedString, Sizable, Styled, div, h_flex, img, px,
    v_flex,
};
use crate::theme::Typography as _;

/// The About page: a hero card with the build identity and outbound links, the
/// on-disk config location, and a trademark disclaimer.
pub(super) fn about_page(view: Entity<SettingsView>, copied: bool, pal: Palette) -> SettingPage {
    let hero = SettingGroup::new().item(SettingItem::render(move |_, _, cx| {
        about_hero(&view, copied, pal, cx)
    }));
    let config = SettingGroup::new().item(SettingItem::render(move |_, _, _| about_config(pal)));
    let footer = SettingGroup::new().item(SettingItem::render(move |_, _, _| {
        div()
            .text_caption()
            .text_color(pal.text_muted)
            .child(tr!(
                "Not affiliated with Logitech. \"Logitech\", \"MX Master\", and \"Options+\" are trademarks of Logitech International S.A."
            ))
            .into_any_element()
    }));

    SettingPage::new(tr!("About"))
        .icon(IconName::Info)
        .resettable(false)
        .description(tr!(
            "A native, local-first alternative to Logitech Options+."
        ))
        .group(hero)
        .group(config)
        .group(footer)
}

/// The About hero row: logo, wordmark, the clickable build line, and the link /
/// diagnostics buttons.
fn about_hero(view: &Entity<SettingsView>, copied: bool, pal: Palette, _: &mut App) -> AnyElement {
    let diag_label = if copied {
        tr!("Copied!")
    } else {
        tr!("Copy Diagnostics")
    };
    let view = view.clone();

    h_flex()
        .w_full()
        .items_start()
        .gap_3()
        .child(img(crate::app_assets::LOGO).w(px(56.)).h(px(56.)))
        .child(
            v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::BOLD)
                                .child("OpenLogi"),
                        )
                        .child(
                            div()
                                .text_body()
                                .text_color(pal.text_muted)
                                .child(env!("CARGO_PKG_VERSION")),
                        ),
                )
                .child(
                    h_flex()
                        .items_center()
                        .gap_1()
                        .pt_1()
                        .child(link_button(
                            "about-repo",
                            Icon::new(IconName::Github),
                            tr!("GitHub"),
                            REPO_URL,
                        ))
                        .child(link_button(
                            "about-changelog",
                            Icon::empty().path("action-icons/scroll-text.svg"),
                            tr!("Changelog"),
                            RELEASES_URL,
                        ))
                        .child(link_button(
                            "about-docs",
                            Icon::new(IconName::BookOpen),
                            tr!("Documentation"),
                            HELP_URL,
                        ))
                        .child(link_button(
                            "about-issue",
                            Icon::empty().path("action-icons/bug.svg"),
                            tr!("Report an issue"),
                            format!("{REPO_URL}/issues"),
                        ))
                        .child(div().w(px(1.)).h(px(16.)).mx_1().bg(pal.border))
                        .child(
                            Button::new("about-copy-diagnostics")
                                .ghost()
                                .small()
                                .icon(IconName::Copy)
                                .label(diag_label)
                                .on_click(move |_, _, cx| {
                                    let report = crate::diagnostics::collect(cx).to_markdown();
                                    cx.write_to_clipboard(ClipboardItem::new_string(report));
                                    view.update(cx, |this, cx| {
                                        this.copied = true;
                                        this.copied_gen = this.copied_gen.wrapping_add(1);
                                        let generation = this.copied_gen;
                                        cx.notify();
                                        cx.spawn(async move |handle, cx| {
                                            cx.background_executor()
                                                .timer(std::time::Duration::from_secs(2))
                                                .await;
                                            handle
                                                .update(cx, |this, cx| {
                                                    if this.copied_gen == generation {
                                                        this.copied = false;
                                                        cx.notify();
                                                    }
                                                })
                                                .ok();
                                        })
                                        .detach();
                                    });
                                }),
                        ),
                ),
        )
        .into_any_element()
}

/// The config-file location row with a reveal-in-file-manager button.
fn about_config(pal: Palette) -> AnyElement {
    let path = openlogi_core::paths::config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            // The absolute path can be arbitrarily long (deep home dirs,
            // Windows profiles) — ellipsize it rather than letting it shove
            // the reveal button past the window edge.
            v_flex()
                .gap_1()
                .flex_1()
                .min_w_0()
                .child(div().font_weight(FontWeight::MEDIUM).child("config.toml"))
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .truncate()
                        .child(path),
                ),
        )
        .child(
            div().flex_shrink_0().child(
                Button::new("about-reveal-config")
                    .outline()
                    .label(tr!("Show in file manager"))
                    .on_click(|_, _, cx| {
                        if let Ok(dir) = openlogi_core::paths::config_dir()
                            && let Ok(url) = url::Url::from_file_path(&dir)
                        {
                            cx.open_url(url.as_str());
                        }
                    }),
            ),
        )
        .into_any_element()
}

/// A subtle ghost button with a leading icon that opens `href`, used for the
/// About link row.
fn link_button(
    id: &'static str,
    icon: Icon,
    label: SharedString,
    href: impl Into<SharedString>,
) -> Button {
    let href = href.into();
    Button::new(id)
        .ghost()
        .small()
        .icon(icon)
        .label(label)
        .on_click(move |_, _, cx| cx.open_url(&href))
}
