//! The Settings window — a standalone OS window (⌘, / menu bar / the right
//! panel's Configuration card) exposing the app-wide preferences in
//! [`openlogi_core::config::AppSettings`].
//!
//! Uses gpui-component's Settings widget so page navigation, search, and the
//! left sidebar share the same behaviour as the rest of that component set.

// `.on_click` on the `.id(...)`-stateful asset action buttons (and the macOS
// permission rows) needs this on every platform, so it isn't macOS-gated.
use gpui::StatefulInteractiveElement as _;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use gpui::rgb;
use gpui::{
    AnyElement, App, AppContext as _, BorrowAppContext as _, Context, Entity, FocusHandle,
    InteractiveElement, IntoElement, ParentElement as _, Render, SharedString, Size, Styled as _,
    Subscription, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    IconName, IndexPath, Sizable, h_flex,
    select::{Select, SelectEvent, SelectItem, SelectState},
    setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings},
    slider::{Slider, SliderEvent, SliderState},
    v_flex,
};
use openlogi_core::config::{
    DEFAULT_THUMBWHEEL_SENSITIVITY, MAX_THUMBWHEEL_SENSITIVITY, MIN_THUMBWHEEL_SENSITIVITY,
};

use crate::app_menu::{CloseWindow, Minimize, Zoom};
#[cfg(target_os = "macos")]
use crate::platform::permissions::Permission;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use crate::platform::permissions::{self, PermissionStatus};
use crate::state::AppState;
use crate::theme::{self, Palette};
use crate::windows::{self, AuxWindow};
use crate::{AssetCommand, AssetControl};

/// Standalone Settings window root view.
pub struct SettingsView {
    focus_handle: FocusHandle,
    #[allow(dead_code, reason = "held to keep the appearance observer alive")]
    appearance_obs: Option<Subscription>,
    language_select: Entity<SelectState<Vec<LanguageOption>>>,
    sensitivity_slider: Entity<SliderState>,
    /// Asset-cache size blurb, computed once when the window opens rather than
    /// re-walking the cache on every render. A snapshot — reopen to refresh
    /// after a Clear.
    asset_cache_desc: SharedString,
}

impl SettingsView {
    #[allow(
        clippy::cast_precision_loss,
        reason = "sensitivity bounds are tiny 1..=100 integers — exact in f32"
    )]
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let current = cx
            .try_global::<AppState>()
            .and_then(|s| s.app_settings().language.clone());
        let options = language_options();
        let selected = selected_language_index(current.as_deref(), &options);
        let language_select = cx.new(|cx| SelectState::new(options, Some(selected), window, cx));
        cx.subscribe_in(&language_select, window, Self::on_language_select)
            .detach();

        let sensitivity = cx
            .try_global::<AppState>()
            .map_or(DEFAULT_THUMBWHEEL_SENSITIVITY, |s| {
                s.app_settings().thumbwheel_sensitivity
            });
        let sensitivity_slider = cx.new(|_| {
            SliderState::new()
                .min(MIN_THUMBWHEEL_SENSITIVITY as f32)
                .max(MAX_THUMBWHEEL_SENSITIVITY as f32)
                .default_value(sensitivity as f32)
        });
        cx.subscribe_in(&sensitivity_slider, window, Self::on_sensitivity_slider)
            .detach();

        Self {
            focus_handle,
            appearance_obs: None,
            language_select,
            sensitivity_slider,
            asset_cache_desc: cache_size_description(),
        }
    }

    /// Commit the thumb-wheel sensitivity slider. The label tracks the live
    /// slider value on every `Change`; persistence (and the one shared-atomic
    /// write the watcher reads) happens once on `Release`.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "slider value is a stepped 1..=100 figure"
    )]
    #[allow(
        clippy::unused_self,
        reason = "gpui subscription handlers must take &mut self"
    )]
    fn on_sensitivity_slider(
        &mut self,
        _: &Entity<SliderState>,
        event: &SliderEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let SliderEvent::Release(value) = event {
            let sensitivity = value.start().round() as i32;
            cx.update_global::<AppState, _>(|s, _| s.set_thumbwheel_sensitivity(sensitivity));
        }
        cx.notify();
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

        cx.update_global::<AppState, _>(|s, cx| s.set_language(language, cx));
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
        Size::new(px(820.), px(520.)),
        SettingsView::new,
        cx,
    );
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);

        div()
            .size_full()
            .bg(pal.bg)
            .text_color(pal.text_primary)
            .track_focus(&self.focus_handle)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            .child(
                Settings::new("settings")
                    .sidebar_width(px(210.))
                    .page(general_page(self.sensitivity_slider.clone()))
                    .page(permissions_page(pal))
                    .page(assets_page(pal, self.asset_cache_desc.clone()))
                    .page(language_page(self.language_select.clone())),
            )
    }
}

fn general_page(sensitivity_slider: Entity<SliderState>) -> SettingPage {
    let group = SettingGroup::new()
        .item(
            SettingItem::new(
                tr!("Thumb Wheel Sensitivity"),
                SettingField::render(move |_, _, cx| {
                    sensitivity_field(&sensitivity_slider, cx)
                }),
            )
            .description(tr!(
                "Scales the thumb wheel's horizontal scroll speed and how readily custom wheel actions trigger."
            )),
        )
        .item(
            SettingItem::new(
                tr!("Launch at login"),
                SettingField::switch(
                    |cx| {
                        cx.try_global::<AppState>()
                            .is_some_and(|s| s.app_settings().launch_at_login)
                    },
                    |enabled, cx| {
                        cx.update_global::<AppState, _>(move |s, _| {
                            s.set_launch_at_login(enabled);
                        });
                        cx.refresh_windows();
                    },
                ),
            )
            .description(tr!(
                "Automatically start OpenLogi when you log in."
            )),
        )
        .item(
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

    #[cfg(target_os = "macos")]
    let group = group.item(
        SettingItem::new(
            tr!("Show in menu bar"),
            SettingField::switch(
                |cx| {
                    cx.try_global::<AppState>()
                        .is_some_and(|s| s.app_settings().show_in_menu_bar)
                },
                |enabled, cx| {
                    cx.update_global::<AppState, _>(move |s, _| {
                        s.set_show_in_menu_bar(enabled);
                    });
                    cx.refresh_windows();
                },
            ),
        )
        .description(tr!(
            "Keep OpenLogi's icon in the menu bar. When off, it stays in the Dock instead."
        )),
    );

    SettingPage::new(tr!("General"))
        .icon(IconName::Settings)
        .resettable(false)
        .group(group)
}

#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux")),
    allow(unused_variables)
)]
fn permissions_page(pal: Palette) -> SettingPage {
    let page = SettingPage::new(tr!("Permissions"))
        .icon(IconName::Info)
        .resettable(false);

    #[cfg(target_os = "macos")]
    let page = page.group(
        SettingGroup::new()
            .item(permission_item(
                "perm-accessibility",
                tr!("Accessibility"),
                tr!("Needed for gesture and button remapping (event tap)."),
                Permission::Accessibility,
                |cx| {
                    // The agent owns the hook, so this is *its* grant,
                    // reported over IPC; while not connected the state is
                    // genuinely unknown, not denied.
                    match cx.try_global::<AppState>().and_then(AppState::agent_status) {
                        Some(status) if status.accessibility_granted => PermissionStatus::Granted,
                        Some(_) => PermissionStatus::Denied,
                        None => PermissionStatus::Unknown,
                    }
                },
                pal,
            ))
            .item(permission_item(
                "perm-input-monitoring",
                tr!("Input Monitoring"),
                tr!("Needed to read HID++ data, including Bluetooth-direct mice."),
                Permission::InputMonitoring,
                |_| permissions::input_monitoring(),
                pal,
            ))
            .item(permission_item(
                "perm-bluetooth",
                tr!("Bluetooth"),
                tr!("Allows OpenLogi to use CoreBluetooth (not required for HID access)."),
                Permission::Bluetooth,
                |_| permissions::bluetooth(),
                pal,
            )),
    );

    #[cfg(target_os = "linux")]
    let page = page.group(SettingGroup::new().item({
        // Description is only shown when access is not yet granted — no noise
        // when everything is already working.
        SettingItem::new(
            tr!("Input device access"),
            SettingField::render(move |_, _, _| {
                let status = permissions::input_device_access();
                let field = gpui_component::v_flex().gap_1().child(status_badge(status));
                let hint = match status {
                    PermissionStatus::Denied => Some(tr!(
                        "OpenLogi needs write access to /dev/uinput (for button \
                         remapping) and read/write access to /dev/hidraw* (for HID++ \
                         communication). Install the OpenLogi udev rules to grant \
                         access — see the Linux install guide."
                    )),
                    PermissionStatus::Unknown => Some(tr!(
                        "No Logitech device detected. Connect your device or verify \
                         the hidraw udev rules are installed."
                    )),
                    PermissionStatus::Granted => None,
                };
                if let Some(text) = hint {
                    field.child(div().text_xs().text_color(pal.text_muted).child(text))
                } else {
                    field
                }
            }),
        )
    }));

    page
}

#[cfg(target_os = "macos")]
fn permission_item(
    id: &'static str,
    title: SharedString,
    description: SharedString,
    permission: Permission,
    status: impl Fn(&App) -> PermissionStatus + 'static,
    pal: Palette,
) -> SettingItem {
    SettingItem::new(
        title,
        SettingField::render(move |_, _, cx| permission_field(id, status(cx), permission, pal)),
    )
    .description(description)
}

fn assets_page(pal: Palette, cache_desc: SharedString) -> SettingPage {
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
fn cache_size_description() -> SharedString {
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

fn language_page(language_select: Entity<SelectState<Vec<LanguageOption>>>) -> SettingPage {
    SettingPage::new(tr!("Language"))
        .icon(IconName::Globe)
        .resettable(false)
        .group(
            SettingGroup::new().item(
                SettingItem::new(
                    tr!("Language"),
                    SettingField::render(move |_, _, _| {
                        language_select_field(language_select.clone())
                    }),
                )
                .description(tr!("Choose the interface language.")),
            ),
        )
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

/// A coloured status word for a permission row.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn status_badge(status: PermissionStatus) -> impl IntoElement {
    let (label, color) = match status {
        PermissionStatus::Granted => (tr!("Granted"), theme::STATUS_CONNECTED),
        PermissionStatus::Denied => (tr!("Not granted"), theme::STATUS_CONNECTING),
        PermissionStatus::Unknown => (tr!("Unknown"), theme::STATUS_OFFLINE),
    };
    div().text_xs().text_color(rgb(color)).child(label)
}

/// The right-side field for one permission row: live status, plus (macOS only)
/// an "Open" button that deep-links to the relevant System Settings pane.
#[cfg(target_os = "macos")]
fn permission_field(
    id: &'static str,
    status: PermissionStatus,
    permission: Permission,
    pal: Palette,
) -> impl IntoElement {
    let row = h_flex()
        .flex_shrink_0()
        .items_center()
        .gap_3()
        .child(status_badge(status));

    #[cfg(target_os = "macos")]
    let row = row.child(
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(pal.border)
            .text_xs()
            .cursor_pointer()
            .hover(move |s| s.bg(pal.surface_hover))
            .child(tr!("Open"))
            .on_click(move |_, _, cx| {
                // Accessibility must be prompted in the agent (it owns the
                // hook); prompting in the GUI would authorize the wrong
                // binary. Other panes just deep-link to System Settings.
                if matches!(permission, Permission::Accessibility)
                    && let Some(state) = cx.try_global::<crate::state::AppState>()
                {
                    state.request_accessibility_prompt();
                }
                permissions::open_pane(permission);
            }),
    );

    #[cfg(not(target_os = "macos"))]
    let _ = (id, permission, pal);

    row
}

/// The language picker field. "Follow system" clears the stored preference
/// (`None`); explicit locale entries come from [`crate::i18n::SUPPORTED`].
#[allow(
    clippy::needless_pass_by_value,
    reason = "built inside an `Fn` render closure, so a `&Entity` parameter would make \
              the returned element borrow a captured variable; `Entity` is a cheap handle"
)]
fn language_select_field(
    language_select: Entity<SelectState<Vec<LanguageOption>>>,
) -> impl IntoElement {
    // The Select's root is `size_full`, so pin it to a fixed-size box instead
    // of letting it consume the whole Settings item row.
    div().flex_shrink_0().w(px(220.)).h_6().child(
        Select::new(&language_select)
            .small()
            .w(px(220.))
            .menu_width(px(220.)),
    )
}

/// The thumb-wheel sensitivity field: the slider plus a live value readout that
/// flags the 1× default. Reads the slider entity directly so the readout tracks
/// the drag; persistence is handled by [`SettingsView::on_sensitivity_slider`].
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "slider value is a stepped 1..=100 figure"
)]
fn sensitivity_field(slider: &Entity<SliderState>, cx: &mut App) -> AnyElement {
    let value = slider.read(cx).value().start().round() as i32;
    let is_default = value == DEFAULT_THUMBWHEEL_SENSITIVITY;
    let pal = theme::palette(cx);
    v_flex()
        .flex_shrink_0()
        .gap_1()
        .child(
            h_flex()
                .items_center()
                .gap_3()
                .child(div().w(px(180.)).child(Slider::new(slider)))
                .child(
                    div()
                        .w(px(72.))
                        .text_sm()
                        .text_color(pal.text_muted)
                        .child(value.to_string()),
                ),
        )
        .when(is_default, |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(pal.text_muted)
                    .whitespace_nowrap()
                    .child(format!("({})", rust_i18n::t!("Default"))),
            )
        })
        .into_any_element()
}
