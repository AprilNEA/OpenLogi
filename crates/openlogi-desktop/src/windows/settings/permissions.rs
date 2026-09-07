//! Permissions settings page (macOS / Linux).

#[cfg(target_os = "macos")]
use super::{App, AppState, InteractiveElement, Permission};
use super::{IconName, Palette, SettingPage};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use super::{
    ParentElement, PermissionStatus, SettingField, SettingGroup, SettingItem, SharedString, Styled,
    div, h_flex, px, rgb, theme,
};
use crate::ui::theme::Typography as _;
#[cfg(target_os = "macos")]
use gpui_base::Button as BaseButton;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use openlogi_permissions as permissions;

#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux")),
    expect(
        unused_variables,
        reason = "`has_camera` only gates a macOS/Linux row; elsewhere the page is empty"
    )
)]
pub(super) fn permissions_page(has_camera: bool) -> SettingPage {
    let page = SettingPage::new(tr!("permissions.permissions"))
        .icon(IconName::Info)
        .resettable(false);

    #[cfg(target_os = "macos")]
    let page = {
        let mut group = SettingGroup::new()
            .item(permission_item(
                "perm-accessibility",
                tr!("permissions.accessibility"),
                tr!("permissions.accessibility_permission_requirement"),
                Permission::Accessibility,
                |cx| {
                    // The agent owns the hook, so this is *its* grant,
                    // reported over IPC; while not connected the state is
                    // genuinely unknown, not denied.
                    match AppState::try_global(cx)
                        .map(|state| state.read(cx))
                        .and_then(AppState::agent_status)
                    {
                        Some(status) if status.accessibility_granted => PermissionStatus::Granted,
                        Some(_) => PermissionStatus::Denied,
                        None => PermissionStatus::Unknown,
                    }
                },
            ))
            .item(input_monitoring_item())
            .item(permission_item(
                "perm-bluetooth",
                tr!("permissions.bluetooth"),
                tr!("permissions.bluetooth_permission_description"),
                Permission::Bluetooth,
                |cx| match AppState::try_global(cx)
                    .map(|state| state.read(cx))
                    .and_then(AppState::agent_status)
                {
                    Some(status) if status.bluetooth_granted => PermissionStatus::Granted,
                    Some(_) => PermissionStatus::Denied,
                    None => PermissionStatus::Unknown,
                },
            ));
        // Camera access is only worth asking for once a Logitech webcam is
        // actually connected — it then appears on the main page, and granting
        // access turns on its live preview.
        if has_camera {
            group = group.item(permission_item(
                "perm-camera",
                tr!("camera.camera"),
                tr!("permissions.camera_permission_description"),
                Permission::Camera,
                |_| permissions::camera(),
            ));
        }
        page.group(group)
    };

    #[cfg(not(target_os = "macos"))]
    let _ = has_camera;

    #[cfg(target_os = "linux")]
    let page = page.group(SettingGroup::new().item({
        // Description is only shown when access is not yet granted — no noise
        // when everything is already working.
        SettingItem::new(
            tr!("permissions.input_device_access"),
            SettingField::render(move |_, _, cx| {
                let pal = theme::palette(cx);
                let status = permissions::input_device_access();
                let field = gpui_component::v_flex()
                    .gap_1()
                    .child(status_badge(status, pal));
                let hint = match status {
                    PermissionStatus::Denied => {
                        Some(tr!("permissions.linux_input_access_denied_description"))
                    }
                    PermissionStatus::Unknown => {
                        Some(tr!("permissions.linux_input_access_unknown_description"))
                    }
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
fn input_monitoring_item() -> SettingItem {
    SettingItem::new(
        tr!("permissions.input_monitoring"),
        SettingField::render(move |_, _, cx| {
            let status = AppState::try_global(cx)
                .map(|state| state.read(cx))
                .and_then(AppState::agent_status);
            // Granted-but-still-failing is the one state the badge
            // alone cannot express: the grant exists, yet every
            // open is refused — an exclusive open elsewhere, or a
            // TCC session only a re-login refreshes (#704).
            let stalled = status
                .as_ref()
                .is_some_and(|s| s.input_monitoring_granted && s.hid_open_failures);
            let badge = match status {
                Some(s) if s.input_monitoring_granted => PermissionStatus::Granted,
                Some(_) => PermissionStatus::Denied,
                None => PermissionStatus::Unknown,
            };
            let field = gpui_component::v_flex().gap_1().child(permission_field(
                "perm-input-monitoring",
                badge,
                Permission::InputMonitoring,
                cx,
            ));
            let pal = theme::palette(cx);
            if stalled {
                field.child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(tr!("permissions.input_monitoring_granted_but_unavailable")),
                )
            } else {
                field
            }
        }),
    )
    .description(tr!("permissions.input_monitoring_permission_description"))
}

#[cfg(target_os = "macos")]
fn permission_item(
    id: &'static str,
    title: SharedString,
    description: SharedString,
    permission: Permission,
    status: impl Fn(&App) -> PermissionStatus + 'static,
) -> SettingItem {
    SettingItem::new(
        title,
        SettingField::render(move |_, _, cx| permission_field(id, status(cx), permission, cx)),
    )
    .description(description)
}

/// A readable status word with colour retained as a supplemental marker.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn status_badge(status: PermissionStatus, pal: Palette) -> gpui::Div {
    let (label, color) = match status {
        PermissionStatus::Granted => (tr!("permissions.granted"), theme::STATUS_CONNECTED),
        PermissionStatus::Denied => (tr!("permissions.not_granted"), theme::STATUS_CONNECTING),
        PermissionStatus::Unknown => (tr!("permissions.unknown"), theme::STATUS_OFFLINE),
    };
    badge(label, color, pal)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn badge(label: SharedString, color: u32, pal: Palette) -> gpui::Div {
    h_flex()
        .items_center()
        .gap_1()
        .text_caption()
        .text_color(pal.text_primary)
        .child(div().size(px(6.)).rounded_full().bg(rgb(color)))
        .child(label)
}

/// The right-side field for one permission row: live status plus an action button (a System Settings deep link, or the Camera consent prompt).
#[cfg(target_os = "macos")]
fn permission_field(
    id: &'static str,
    status: PermissionStatus,
    permission: Permission,
    cx: &App,
) -> gpui::Div {
    let pal = theme::palette(cx);
    // Camera stays "Not requested" until this process has asked AVFoundation.
    let never_requested =
        matches!(status, PermissionStatus::Unknown) && matches!(permission, Permission::Camera);
    let status_el = if never_requested {
        badge(tr!("permissions.not_requested"), theme::STATUS_OFFLINE, pal)
    } else {
        status_badge(status, pal)
    };
    let action_label = if matches!(status, PermissionStatus::Granted) {
        tr!("common.open")
    } else {
        tr!("permissions.grant")
    };

    h_flex()
        .flex_shrink_0()
        .items_center()
        .gap_3()
        .child(status_el)
        .child(
            BaseButton::new(id)
                .accessibility_label(action_label.clone())
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
                .child(action_label)
                .on_click(move |_, _, cx| {
                    if matches!(permission, Permission::Camera) {
                        if never_requested {
                            crate::features::camera::request_camera_access(cx);
                        } else {
                            permissions::open_pane(permission);
                        }
                        return;
                    }
                    if matches!(status, PermissionStatus::Granted) {
                        permissions::open_pane(permission);
                        return;
                    }
                    // The agent owns these grants; prompting here would
                    // authorize the GUI instead of OpenLogi Agent.
                    if let Some(state) = crate::state::AppState::try_global(cx) {
                        match permission {
                            Permission::Accessibility => {
                                state.read(cx).request_accessibility_prompt(true);
                            }
                            Permission::InputMonitoring => {
                                state.read(cx).request_input_monitoring_prompt(true);
                            }
                            Permission::Bluetooth => {
                                state.read(cx).request_bluetooth_prompt(true);
                            }
                            Permission::Camera => {
                                permissions::open_pane(permission);
                            }
                        }
                    } else {
                        permissions::open_pane(permission);
                    }
                }),
        )
}
