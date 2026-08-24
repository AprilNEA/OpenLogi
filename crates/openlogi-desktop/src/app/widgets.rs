//! Small leaf UI pieces shared between the Home and device-detail screens:
//! panel chrome, status pills, and the header buttons that appear on both
//! screens.

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div, px, relative, rgb,
};
use gpui_component::{
    IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use openlogi_core::device::{BatteryInfo, BatteryStatus, DeviceKind};
use openlogi_core::hid::DeviceRoute;

use super::AppView;
use crate::state::AppState;
use crate::ui::theme::{self, Palette, Typography as _};

/// True when the device is charging but still reports 0% — the MX2S `0x1000`
/// firmware can't gauge charge under load, and on a cold start there's no
/// pre-charge % cached to carry forward. Show "Charging" without the bogus 0%.
pub(crate) fn battery_charging_no_reading(b: &BatteryInfo) -> bool {
    matches!(
        b.status,
        BatteryStatus::Charging | BatteryStatus::ChargingSlow
    ) && b.percentage == 0
}

/// "← Back" affordance on the detail screen; returns to the gallery without
/// changing the active-device selection.
pub(super) fn back_button(cx: &mut Context<AppView>) -> impl IntoElement {
    let view = cx.entity();
    Button::new("detail-back")
        .ghost()
        .small()
        .icon(IconName::ChevronLeft)
        .label(tr!("Back"))
        .on_click(move |_, _, cx| view.update(cx, AppView::go_home))
}

/// Square Settings gear in the Home header: opens the Settings window.
pub(super) fn settings_button() -> impl IntoElement {
    Button::new("home-settings")
        .icon(IconName::Settings)
        .tooltip(tr!("Settings"))
        .on_click(|_, _, cx| crate::windows::settings::open(cx))
}

/// Trailing "+" button that opens the pairing window. Present in both screen
/// headers; the empty state carries its own primary "Add Device" CTA, so this
/// never floats alone in an empty header.
pub(super) fn add_device_button() -> impl IntoElement {
    Button::new("header-add-device")
        .icon(IconName::Plus)
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
    let (label, color) = if online {
        (tr!("Connected"), theme::STATUS_CONNECTED)
    } else {
        (tr!("Offline"), theme::STATUS_OFFLINE)
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
        .child(div().size_1p5().rounded_full().bg(rgb(color)))
        .child(label)
}

pub(super) fn battery_summary(battery: &BatteryInfo, pal: Palette) -> impl IntoElement {
    let status = match battery.status {
        BatteryStatus::Charging | BatteryStatus::ChargingSlow => tr!("Charging"),
        BatteryStatus::Full => tr!("Full"),
        BatteryStatus::Error => tr!("Battery error"),
        BatteryStatus::Discharging | BatteryStatus::Unknown => tr!("Battery"),
    };
    v_flex()
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .text_caption()
                .text_color(pal.text_muted)
                .child(status)
                .child(if battery_charging_no_reading(battery) {
                    String::new()
                } else {
                    format!("{}%", battery.percentage)
                }),
        )
        .child({
            let track = div()
                .h(px(6.))
                .w_full()
                .rounded_full()
                .bg(pal.surface_hover);
            // Charging with no reliable %: leave the track empty rather than
            // drawing the 1%-wide red critical sliver that percentage==0 yields.
            if battery_charging_no_reading(battery) {
                track
            } else {
                track.child(
                    div()
                        .h_full()
                        .w(relative(f32::from(battery.percentage.clamp(1, 100)) / 100.))
                        .rounded_full()
                        .bg(rgb(battery_color(battery.percentage))),
                )
            }
        })
}

fn battery_color(percentage: u8) -> u32 {
    match percentage {
        0..=20 => 0x00ef_4444,
        21..=50 => theme::STATUS_CONNECTING,
        _ => theme::STATUS_CONNECTED,
    }
}

pub(super) fn sidebar_action(
    id: &'static str,
    icon: IconName,
    label: SharedString,
    handler: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> AnyElement {
    Button::new(id)
        .small()
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
