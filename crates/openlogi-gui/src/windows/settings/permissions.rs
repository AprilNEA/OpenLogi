//! Permissions settings page (macOS / Linux).

use super::{
    App, AppState, InteractiveElement, Permission, SharedString, StatefulInteractiveElement, h_flex,
};
use super::{IconName, Palette, SettingPage};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use super::{
    IntoElement, ParentElement, PermissionStatus, SettingField, SettingGroup, SettingItem, Styled,
    div, rgb, theme,
};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use crate::platform::permissions;
#[cfg(target_os = "macos")]
use crate::theme::Typography as _;

#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux")),
    allow(unused_variables)
)]
pub(super) fn permissions_page(pal: Palette) -> SettingPage {
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
                    field.child(div().text_caption().text_color(pal.text_muted).child(text))
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

/// A coloured status word for a permission row.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn status_badge(status: PermissionStatus) -> impl IntoElement {
    let (label, color) = match status {
        PermissionStatus::Granted => (tr!("Granted"), theme::STATUS_CONNECTED),
        PermissionStatus::Denied => (tr!("Not granted"), theme::STATUS_CONNECTING),
        PermissionStatus::Unknown => (tr!("Unknown"), theme::STATUS_OFFLINE),
    };
    div().text_caption().text_color(rgb(color)).child(label)
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
            .rounded(pal.control_radius)
            .border_1()
            .border_color(pal.border)
            .text_caption()
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
