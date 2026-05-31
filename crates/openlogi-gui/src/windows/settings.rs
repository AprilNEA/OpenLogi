//! The Settings window — a standalone OS window (⌘, / menu / footer link)
//! exposing the app-wide preferences in [`openlogi_core::config::AppSettings`].
//!
//! Two toggles for now, so the layout is a hand-rolled form rather than
//! gpui-component's [`Settings`](gpui_component::setting::Settings) widget
//! (whose 250px page sidebar would dwarf two switches). When the preference
//! set grows enough to warrant pages, this can migrate to that widget.

use gpui::{
    App, BorrowAppContext as _, Context, FontWeight, InteractiveElement, IntoElement,
    ParentElement as _, Render, SharedString, Size, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, px, rgb,
};
use gpui_component::{
    Icon, IconName, group_box::GroupBox, h_flex, scroll::ScrollableElement, switch::Switch, v_flex,
};

use crate::state::AppState;
use crate::theme::{self, Palette};
use crate::windows::{self, AuxWindow};

/// Standalone Settings window root view.
pub struct SettingsView {
    #[allow(dead_code, reason = "held to keep the appearance observer alive")]
    appearance_obs: Option<Subscription>,
}

impl SettingsView {
    fn new(_: &mut Context<Self>) -> Self {
        Self {
            appearance_obs: None,
        }
    }
}

impl AuxWindow for SettingsView {
    fn set_appearance_obs(&mut self, sub: Subscription) {
        self.appearance_obs = Some(sub);
    }
}

/// Open the Settings window, or focus it if it's already open.
pub fn open(cx: &mut App) {
    windows::open_or_focus(
        |reg| &mut reg.settings,
        "Settings",
        Size::new(px(520.), px(360.)),
        SettingsView::new,
        cx,
    );
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let (launch, updates, language) =
            cx.try_global::<AppState>()
                .map_or((false, false, None), |s| {
                    let a = s.app_settings();
                    (a.launch_at_login, a.check_for_updates, a.language.clone())
                });

        let general = GroupBox::new()
            .title(group_title(IconName::Settings, tr!("General")))
            .child(setting_row(
                Switch::new("launch-at-login")
                    .checked(launch)
                    .on_click(cx.listener(|_, checked: &bool, _, cx| {
                        let enabled = *checked;
                        cx.update_global::<AppState, _>(move |s, _| {
                            s.set_launch_at_login(enabled);
                        });
                        cx.notify();
                    })),
                tr!("Launch at login"),
                tr!("Automatically start OpenLogi when you log in to macOS."),
                pal,
            ))
            .child(setting_row(
                Switch::new("check-for-updates")
                    .checked(updates)
                    .on_click(cx.listener(|_, checked: &bool, _, cx| {
                        let enabled = *checked;
                        cx.update_global::<AppState, _>(move |s, _| {
                            s.set_check_for_updates(enabled);
                        });
                        cx.notify();
                    })),
                tr!("Check for updates"),
                tr!(
                    "Check once per launch for a new version (query only — no automatic download)."
                ),
                pal,
            ));

        // The menu-bar (status item) is macOS-only, so its toggle is too.
        #[cfg(target_os = "macos")]
        let general = {
            let in_menu_bar = cx
                .try_global::<AppState>()
                .is_some_and(|s| s.app_settings().show_in_menu_bar);
            general.child(setting_row(
                Switch::new("show-in-menu-bar")
                    .checked(in_menu_bar)
                    .on_click(cx.listener(|_, checked: &bool, _, cx| {
                        let enabled = *checked;
                        cx.update_global::<AppState, _>(move |s, _| {
                            s.set_show_in_menu_bar(enabled);
                        });
                        cx.notify();
                    })),
                tr!("Show in menu bar"),
                tr!(
                    "Keep OpenLogi's icon in the menu bar. When off, it stays in the Dock instead."
                ),
                pal,
            ))
        };

        v_flex()
            .size_full()
            .bg(pal.bg)
            .text_color(pal.text_primary)
            .child(
                v_flex()
                    .w_full()
                    .p_6()
                    .gap_6()
                    .overflow_y_scrollbar()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr!("Settings")),
                    )
                    .child(general)
                    .child(
                        GroupBox::new()
                            .title(group_title(IconName::Globe, tr!("Language")))
                            .child(language_row(language.as_deref(), pal, cx)),
                    ),
            )
    }
}

/// A GroupBox title with a small leading icon. `GroupBox::title` styles the
/// text itself, so this only lays the icon and label out inline.
fn group_title(icon: IconName, label: SharedString) -> impl IntoElement {
    h_flex()
        .gap_1p5()
        .items_center()
        .child(Icon::new(icon))
        .child(label)
}

/// One row: title + muted description on the left, the control on the right.
fn setting_row(
    control: Switch,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    pal: Palette,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_4()
        .child(
            v_flex()
                .flex_1()
                .min_w(px(0.))
                .gap_1()
                .child(div().text_sm().child(title.into()))
                .child(
                    div()
                        .text_xs()
                        .text_color(pal.text_muted)
                        .child(description.into()),
                ),
        )
        .child(control)
}

/// The language picker: a muted hint above a wrapping row of locale chips. The
/// leading "Follow system" chip clears the stored preference (`None`); the rest
/// pin an explicit locale from [`crate::i18n::SUPPORTED`]. Selecting one
/// switches the locale live, then repaints every window and the menu bar so the
/// whole UI re-renders without a restart.
fn language_row(
    current: Option<&str>,
    pal: Palette,
    cx: &mut Context<SettingsView>,
) -> impl IntoElement {
    let mut options: Vec<(SharedString, Option<String>)> = vec![(tr!("Follow system"), None)];
    options.extend(
        crate::i18n::SUPPORTED
            .iter()
            .map(|(code, name)| (SharedString::from(*name), Some((*code).to_string()))),
    );

    let chips: Vec<_> = options
        .into_iter()
        .enumerate()
        .map(|(idx, (label, lang))| {
            let active = current == lang.as_deref();
            div()
                .id(("lang-chip", idx))
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(if active {
                    rgb(theme::ACCENT_BLUE).into()
                } else {
                    pal.border
                })
                .text_xs()
                .text_color(if active {
                    rgb(theme::ACCENT_BLUE).into()
                } else {
                    pal.text_primary
                })
                .cursor_pointer()
                .hover(|s| s.bg(pal.surface_hover))
                .child(label)
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.update_global::<AppState, _>(|s, _| s.set_language(lang.clone()));
                    // `t!` reads the locale at render time, so a repaint is what
                    // actually applies the switch; the app menu and status item
                    // aren't in any window's view tree, so re-title them too. The
                    // status item's device line lives on the spawn loop, so ask it
                    // to re-localize the whole menu rather than writing from here.
                    cx.refresh_windows();
                    crate::app_menu::rebuild(cx);
                    #[cfg(target_os = "macos")]
                    crate::platform::tray::request_refresh();
                }))
        })
        .collect();

    v_flex()
        .w_full()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(pal.text_muted)
                .child(tr!("Choose the interface language.")),
        )
        .child(h_flex().gap_2().flex_wrap().children(chips))
}
