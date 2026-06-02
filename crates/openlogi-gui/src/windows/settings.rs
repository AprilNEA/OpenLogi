//! The Settings window — a standalone OS window (⌘, / menu / footer link)
//! exposing the app-wide preferences in [`openlogi_core::config::AppSettings`].
//!
//! Two toggles for now, so the layout is a hand-rolled form rather than
//! gpui-component's [`Settings`](gpui_component::setting::Settings) widget
//! (whose 250px page sidebar would dwarf two switches). When the preference
//! set grows enough to warrant pages, this can migrate to that widget.

use gpui::{
    App, AppContext as _, BorrowAppContext as _, Context, Entity, FontWeight, InteractiveElement,
    IntoElement, ParentElement as _, Render, SharedString, Size, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, px, rgb,
};
use gpui_component::{
    Icon, IconName, IndexPath, Sizable,
    group_box::GroupBox,
    h_flex,
    scroll::ScrollableElement,
    select::{Select, SelectEvent, SelectItem, SelectState},
    switch::Switch,
    v_flex,
};

use crate::platform::permissions::{self, Permission, PermissionStatus};
use crate::state::AppState;
use crate::theme::{self, Palette};
use crate::windows::{self, AuxWindow};

/// Standalone Settings window root view.
pub struct SettingsView {
    #[allow(dead_code, reason = "held to keep the appearance observer alive")]
    appearance_obs: Option<Subscription>,
    language_select: Entity<SelectState<Vec<LanguageOption>>>,
}

impl SettingsView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let current = cx
            .try_global::<AppState>()
            .and_then(|s| s.app_settings().language.clone());
        let options = language_options();
        let selected = selected_language_index(current.as_deref(), &options);
        let language_select = cx.new(|cx| SelectState::new(options, Some(selected), window, cx));
        cx.subscribe_in(&language_select, window, Self::on_language_select)
            .detach();

        Self {
            appearance_obs: None,
            language_select,
        }
    }

    fn on_language_select(
        &mut self,
        _: &Entity<SelectState<Vec<LanguageOption>>>,
        event: &SelectEvent<Vec<LanguageOption>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(_) = event;
        let language = self
            .language_select
            .read(cx)
            .selected_value()
            .copied()
            .filter(|code| !code.is_empty())
            .map(ToOwned::to_owned);

        cx.update_global::<AppState, _>(|s, _| s.set_language(language));
        // `t!` reads the locale at render time, so a repaint is what actually
        // applies the switch; the app menu and status item aren't in any
        // window's view tree, so re-title them too. The status item's device
        // line lives on the spawn loop, so ask it to re-localize the whole menu
        // rather than writing from here.
        cx.refresh_windows();
        crate::app_menu::rebuild(cx);
        #[cfg(target_os = "macos")]
        crate::platform::tray::request_refresh();
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
        let (launch, updates) = cx.try_global::<AppState>().map_or((false, false), |s| {
            let a = s.app_settings();
            (a.launch_at_login, a.check_for_updates)
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
                    .child(permissions_group(pal, cx))
                    .child(
                        GroupBox::new()
                            .title(group_title(IconName::Globe, tr!("Language")))
                            .child(language_row(&self.language_select, pal)),
                    ),
            )
    }
}

#[derive(Clone)]
struct LanguageOption {
    label: &'static str,
    value: &'static str,
    localize_label: bool,
}

impl SelectItem for LanguageOption {
    type Value = &'static str;

    fn title(&self) -> SharedString {
        if self.localize_label {
            SharedString::from(rust_i18n::t!("Follow system").into_owned())
        } else {
            SharedString::from(self.label)
        }
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }
}

fn language_options() -> Vec<LanguageOption> {
    let mut options = vec![LanguageOption {
        label: "Follow system",
        value: "",
        localize_label: true,
    }];
    options.extend(
        crate::i18n::SUPPORTED
            .iter()
            .map(|(code, name)| LanguageOption {
                label: name,
                value: code,
                localize_label: false,
            }),
    );
    options
}

fn selected_language_index(current: Option<&str>, options: &[LanguageOption]) -> IndexPath {
    let value = current.unwrap_or_default();
    let row = options
        .iter()
        .position(|option| option.value == value)
        .unwrap_or_default();
    IndexPath::default().row(row)
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

/// The Permissions group: live macOS permission statuses. Accessibility is
/// watcher-backed (read from [`AppState`]); Input Monitoring and Bluetooth are
/// queried live on each render (both are cheap, no-prompt queries).
fn permissions_group(pal: Palette, cx: &mut Context<SettingsView>) -> impl IntoElement {
    let accessibility = if cx
        .try_global::<AppState>()
        .is_some_and(|s| s.accessibility_granted)
    {
        PermissionStatus::Granted
    } else {
        PermissionStatus::Denied
    };

    GroupBox::new()
        .title(group_title(IconName::Info, tr!("Permissions")))
        .child(permission_row(
            "perm-accessibility",
            tr!("Accessibility"),
            tr!("Needed for gesture and button remapping (event tap)."),
            accessibility,
            Permission::Accessibility,
            pal,
            cx,
        ))
        .child(permission_row(
            "perm-input-monitoring",
            tr!("Input Monitoring"),
            tr!("Needed to read HID++ data, including Bluetooth-direct mice."),
            permissions::input_monitoring(),
            Permission::InputMonitoring,
            pal,
            cx,
        ))
        .child(permission_row(
            "perm-bluetooth",
            tr!("Bluetooth"),
            tr!("Allows OpenLogi to use CoreBluetooth (not required for HID access)."),
            permissions::bluetooth(),
            Permission::Bluetooth,
            pal,
            cx,
        ))
}

/// A coloured status word for a permission row.
fn status_badge(status: PermissionStatus) -> impl IntoElement {
    let (label, color) = match status {
        PermissionStatus::Granted => (tr!("Granted"), theme::STATUS_CONNECTED),
        PermissionStatus::Denied => (tr!("Not granted"), theme::STATUS_CONNECTING),
        PermissionStatus::Unknown => (tr!("Unknown"), theme::STATUS_OFFLINE),
    };
    div().text_xs().text_color(rgb(color)).child(label)
}

/// One permission row: title + muted description on the left; the live status
/// word and an "Open" button (deep-links to the System Settings pane) on the
/// right.
fn permission_row(
    id: &'static str,
    title: SharedString,
    description: SharedString,
    status: PermissionStatus,
    permission: Permission,
    pal: Palette,
    cx: &mut Context<SettingsView>,
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
                .child(div().text_sm().child(title))
                .child(
                    div()
                        .text_xs()
                        .text_color(pal.text_muted)
                        .child(description),
                ),
        )
        .child(
            h_flex()
                .gap_3()
                .items_center()
                .child(status_badge(status))
                .child(
                    div()
                        .id(id)
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(pal.border)
                        .text_xs()
                        .cursor_pointer()
                        .hover(|s| s.bg(pal.surface_hover))
                        .child(tr!("Open"))
                        .on_click(
                            cx.listener(move |_, _, _, _| permissions::open_pane(permission)),
                        ),
                ),
        )
}

/// The language picker. "Follow system" clears the stored preference (`None`);
/// the explicit locale entries come from [`crate::i18n::SUPPORTED`]. Selecting
/// one switches the locale live, then repaints every window and the menu bar so
/// the whole UI re-renders without a restart.
fn language_row(
    language_select: &Entity<SelectState<Vec<LanguageOption>>>,
    pal: Palette,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_4()
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_xs()
                .text_color(pal.text_muted)
                .child(tr!("Choose the interface language.")),
        )
        .child(
            // The Select's root is `size_full`, so it would otherwise claim the
            // whole row and starve the description into one char per line. Pin it
            // to a fixed-size, non-shrinking box (h_6 matches the `.small()` input).
            div().flex_shrink_0().w(px(220.)).h_6().child(
                Select::new(language_select)
                    .small()
                    .w(px(220.))
                    .menu_width(px(220.)),
            ),
        )
}
