//! Small leaf UI pieces shared between the Home and device-detail screens:
//! panel chrome, status pills, and the header buttons that appear on both
//! screens.

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
};
use openlogi_core::device::DeviceKind;
use openlogi_core::hid::DeviceRoute;

use super::AppView;
use crate::state::AppState;
use crate::ui::components::control_button;
use crate::ui::theme::{Palette, Typography as _};

/// "← Devices" affordance on the detail screen; returns to the gallery without
/// changing the active-device selection.
pub(super) fn back_button(cx: &mut Context<AppView>) -> impl IntoElement {
    let view = cx.entity();
    Button::new("detail-back")
        .ghost()
        .small()
        .icon(IconName::ChevronLeft)
        .label(tr!("Devices"))
        .on_click(move |_, _, cx| view.update(cx, AppView::go_home))
}

/// Settings button in the Home header: opens the Settings window. The visible
/// label keeps the action discoverable without requiring hover.
pub(super) fn settings_button() -> impl IntoElement {
    Button::new("home-settings")
        .outline()
        .icon(IconName::Settings)
        .label(tr!("Settings"))
        .tooltip(tr!("Settings"))
        .on_click(|_, _, cx| crate::windows::settings::open(cx))
}

/// Primary action that opens the pairing window. The empty state carries its
/// own equivalent CTA, so this never floats alone in an empty header.
pub(super) fn add_device_button() -> impl IntoElement {
    Button::new("header-add-device")
        .primary()
        .icon(IconName::Plus)
        .label(tr!("Add Device"))
        .tooltip(tr!("Add Device"))
        .on_click(|_, _, cx| crate::windows::add_device::open(cx))
}

pub(super) fn main_window_title(show_device: bool, cx: &Context<AppView>) -> SharedString {
    if !show_device {
        return SharedString::from("OpenLogi");
    }
    AppState::try_global(cx)
        .map(|state| state.read(cx))
        .and_then(AppState::current_record)
        .map_or_else(
            || SharedString::from("OpenLogi"),
            |record| SharedString::from(format!("OpenLogi - {}", record.display_name)),
        )
}

pub(super) fn status_badge(online: bool, pal: Palette) -> impl IntoElement {
    let label = if online {
        tr!("Connected")
    } else {
        tr!("Offline")
    };
    h_flex()
        .gap_1()
        .items_center()
        .rounded_full()
        .border_1()
        .border_color(pal.border)
        .px_2()
        .py_1()
        .text_caption()
        .text_color(pal.text_muted)
        .child(connectivity_dot(online, pal))
        .child(label)
}

/// Neutral connectivity indicator: online is solid and offline is hollow, so
/// the state never depends on hue alone.
pub(super) fn connectivity_dot(online: bool, pal: Palette) -> impl IntoElement {
    div()
        .size_1p5()
        .rounded_full()
        .border_1()
        .border_color(pal.text_muted)
        .when(online, |dot| dot.bg(pal.text_primary))
}

pub(super) fn sidebar_action(
    id: &'static str,
    icon: IconName,
    label: SharedString,
    handler: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> AnyElement {
    control_button(id)
        .icon(icon)
        .label(label)
        .on_click(handler)
        .flex_1()
        .into_any_element()
}

pub(super) fn route_label(route: Option<&DeviceRoute>) -> String {
    match route {
        Some(DeviceRoute::Bolt { .. }) => tr!("Bolt receiver").to_string(),
        Some(DeviceRoute::Unifying { .. }) => tr!("Unifying receiver").to_string(),
        Some(DeviceRoute::Direct { .. } | DeviceRoute::RawHid { .. }) => {
            tr!("Direct connection").to_string()
        }
        None => tr!("Unavailable").to_string(),
    }
}

pub(super) fn kind_label(kind: DeviceKind) -> String {
    match kind {
        DeviceKind::Mouse => tr!("Mouse").to_string(),
        DeviceKind::Keyboard => tr!("Keyboard").to_string(),
        DeviceKind::Numpad => tr!("Numpad").to_string(),
        DeviceKind::Presenter => tr!("Presenter").to_string(),
        DeviceKind::Remote => tr!("Remote").to_string(),
        DeviceKind::Trackball => tr!("Trackball").to_string(),
        DeviceKind::Touchpad => tr!("Touchpad").to_string(),
        DeviceKind::Tablet => tr!("Tablet").to_string(),
        DeviceKind::Gamepad => tr!("Gamepad").to_string(),
        DeviceKind::Joystick => tr!("Joystick").to_string(),
        DeviceKind::Headset => tr!("Headset").to_string(),
        DeviceKind::Camera => tr!("Camera").to_string(),
        DeviceKind::Unknown => tr!("Device").to_string(),
        DeviceKind::Light => tr!("Lighting").to_string(),
    }
}
