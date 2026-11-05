//! Permissions settings page (macOS / Linux).

#[cfg(any(target_os = "macos", target_os = "linux"))]
use super::Axis;
#[cfg(target_os = "linux")]
use super::{
    ActiveTheme, ButtonVariants, ClipboardItem, Context, FluentBuilder, Hsla, IntoElement,
};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use super::{
    App, ParentElement, PermissionStatus, SettingField, SettingGroup, SettingItem, SharedString,
    Styled, div, h_flex, px, rgb, theme,
};
#[cfg(target_os = "macos")]
use super::{AppState, InteractiveElement, Permission};
use super::{Entity, IconName, Palette, SettingPage, SettingsView};
#[cfg(target_os = "linux")]
use crate::ui::components::control_button;
#[cfg(target_os = "macos")]
use crate::ui::theme::FocusRingStyle as _;
use crate::ui::theme::Typography as _;
#[cfg(target_os = "macos")]
use gpui_kit::base::Button as BaseButton;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use openlogi_permissions as permissions;
#[cfg(target_os = "linux")]
use openlogi_permissions::udev::{GrantError, RulesManager};

#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux")),
    expect(
        unused_variables,
        reason = "`has_camera` only gates a macOS/Linux row; elsewhere the page is empty"
    )
)]
pub(super) fn permissions_page(view: Entity<SettingsView>, has_camera: bool) -> SettingPage {
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
                |_| permissions::bluetooth(),
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
    // Only Linux's row starts work that reports back into the view. Elsewhere
    // the handle is consumed here: `let _ = view` would not move it, and the
    // by-value parameter would then trip `clippy::needless_pass_by_value`.
    #[cfg(not(target_os = "linux"))]
    drop(view);

    #[cfg(target_os = "linux")]
    let page = page.group(
        SettingGroup::new().item(
            SettingItem::new(
                tr!("permissions.input_device_access"),
                SettingField::render(move |_, _, cx| input_device_access_field(&view, cx)),
            )
            // Vertical: the hint runs to several sentences, not a compact
            // right-side control — stacked full-width below the title it wraps
            // instead of running past the card edge.
            .layout(Axis::Vertical),
        ),
    );

    page
}

/// Where a Linux user finds the manual fix for missing device access: install
/// the udev rules, reload them, and reconnect the device.
#[cfg(target_os = "linux")]
const DEVICE_ACCESS_GUIDE_URL: &str =
    "https://openlogi.org/docs/troubleshooting#udev-rules-do-not-grant-mouse-input-access";

/// The same fix from inside a Flatpak, which has to run on the host.
#[cfg(target_os = "linux")]
const FLATPAK_GUIDE_URL: &str =
    "https://openlogi.org/docs/troubleshooting#a-flatpak-install-shows-no-devices";

/// The Linux "Grant access…" run: the udev rules installed through pkexec.
#[cfg(target_os = "linux")]
#[derive(Default)]
pub(super) enum InputAccessGrant {
    /// Not attempted since the window opened.
    #[default]
    Idle,
    /// pkexec is running: its dialog is open, or the rules are being installed.
    Running,
    /// How the last attempt ended. Success only means the rules are in place;
    /// the live probe still decides the badge.
    Finished(Result<(), GrantError>),
}

#[cfg(target_os = "linux")]
impl SettingsView {
    /// Install the udev rules through pkexec, off the UI thread. The page
    /// re-probes on the render that reports the outcome.
    fn grant_input_device_access(&mut self, cx: &mut Context<Self>) {
        if matches!(self.input_access_grant, InputAccessGrant::Running) {
            return;
        }
        self.input_access_grant = InputAccessGrant::Running;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async { permissions::udev::grant_input_device_access() })
                .await;
            // The window may have closed while the dialog was open.
            let _ = this.update(cx, |this, cx| {
                this.input_access_grant = InputAccessGrant::Finished(outcome);
                cx.notify();
            });
        })
        .detach();
    }
}

/// Live status of the Linux device nodes, and the fix while access is
/// missing: "Grant access…" installs the udev rules, unless a package manager
/// or sandbox owns them, in which case its own instructions replace the
/// button. Nothing beyond the badge once everything works.
#[cfg(target_os = "linux")]
fn input_device_access_field(view: &Entity<SettingsView>, cx: &App) -> gpui::Div {
    let pal = theme::palette(cx);
    let status = permissions::input_device_access();
    let grant = &view.read(cx).input_access_grant;
    let field = gpui_kit::component::v_flex()
        .w_full()
        .gap_2()
        .child(status_badge(status, pal));
    match status {
        // Right after a fix, say whether anything still needs a restart.
        PermissionStatus::Granted => {
            return field.when(
                matches!(grant, InputAccessGrant::Finished(Ok(()))),
                |field| {
                    field.child(caption(
                        tr!("permissions.permission_applies_without_restart"),
                        pal.text_muted,
                    ))
                },
            );
        }
        PermissionStatus::Unknown => {
            return field
                .child(caption(
                    tr!("permissions.linux_input_access_unknown_description"),
                    pal.text_muted,
                ))
                .child(h_flex().child(guide_button(DEVICE_ACCESS_GUIDE_URL)));
        }
        PermissionStatus::Denied => {}
    }
    let field = field.child(caption(
        tr!("permissions.linux_input_access_denied_description"),
        pal.text_muted,
    ));
    field.child(match permissions::udev::rules_manager() {
        Some(RulesManager::Flatpak) => flatpak_remedy(cx),
        Some(RulesManager::NixOs) => gpui_kit::component::v_flex()
            .gap_2()
            .child(caption(
                tr!("permissions.linux_input_access_nixos"),
                pal.text_muted,
            ))
            .child(h_flex().child(guide_button(DEVICE_ACCESS_GUIDE_URL))),
        None => grant_remedy(view, grant, cx),
    })
}

/// "Grant access…" beside the guide, and what the last run reported.
#[cfg(target_os = "linux")]
fn grant_remedy(view: &Entity<SettingsView>, grant: &InputAccessGrant, cx: &App) -> gpui::Div {
    let view = view.clone();
    let running = matches!(grant, InputAccessGrant::Running);
    gpui_kit::component::v_flex()
        .gap_2()
        .child(
            h_flex()
                .gap_2()
                .child(
                    control_button("perm-input-device-grant")
                        .primary()
                        // A loading button draws its spinner in the icon
                        // slot, so it needs one while running.
                        .when(running, |button| button.icon(IconName::LoaderCircle))
                        .label(tr!("permissions.linux_grant_access"))
                        .loading(running)
                        .on_click(move |_, _, cx| {
                            view.update(cx, SettingsView::grant_input_device_access);
                        }),
                )
                .child(guide_button(DEVICE_ACCESS_GUIDE_URL)),
        )
        .when_some(grant_message(grant, cx), |remedy, (text, color)| {
            remedy.child(caption(text, color))
        })
}

/// A Flatpak cannot reach the host's `/etc` or pkexec, so the fix is a
/// command for the user to run on the host.
#[cfg(target_os = "linux")]
fn flatpak_remedy(cx: &App) -> gpui::Div {
    let pal = theme::palette(cx);
    let command = permissions::udev::flatpak_host_command();
    gpui_kit::component::v_flex()
        .gap_2()
        .child(caption(
            tr!("permissions.linux_input_access_flatpak"),
            pal.text_muted,
        ))
        .when_some(command.clone(), |remedy, command| {
            remedy.child(
                div()
                    .px_2()
                    .py_1()
                    .rounded(pal.control_radius)
                    .border_1()
                    .border_color(pal.border)
                    .bg(pal.control)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_caption()
                    .text_color(pal.text_primary)
                    .child(command),
            )
        })
        .child(
            h_flex()
                .gap_2()
                .when_some(command, |row, command| {
                    row.child(
                        control_button("perm-input-device-copy")
                            .outline()
                            .label(tr!("common.copy"))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(command.clone()));
                            }),
                    )
                })
                .child(guide_button(FLATPAK_GUIDE_URL)),
        )
}

/// What the last "Grant access…" run reports while access is still denied,
/// and the colour that tells a failure from a step to take.
#[cfg(target_os = "linux")]
fn grant_message(grant: &InputAccessGrant, cx: &App) -> Option<(SharedString, Hsla)> {
    let muted = theme::palette(cx).text_muted;
    let danger = cx.theme().danger;
    Some(match grant {
        InputAccessGrant::Idle => return None,
        InputAccessGrant::Running => (tr!("permissions.linux_grant_waiting"), muted),
        // Installed, and still denied: what is left is the user's to do.
        InputAccessGrant::Finished(Ok(())) => (tr!("permissions.linux_grant_still_missing"), muted),
        InputAccessGrant::Finished(Err(GrantError::Cancelled)) => {
            (tr!("permissions.linux_grant_cancelled"), muted)
        }
        InputAccessGrant::Finished(Err(GrantError::PkexecMissing)) => {
            (tr!("permissions.linux_grant_pkexec_missing"), danger)
        }
        InputAccessGrant::Finished(Err(GrantError::Failed(reason))) => (
            tr!("permissions.linux_grant_failed", reason => reason),
            danger,
        ),
    })
}

#[cfg(target_os = "linux")]
fn caption(text: SharedString, color: Hsla) -> gpui::Div {
    div().text_caption().text_color(color).child(text)
}

/// "View guide": the troubleshooting page for doing the fix by hand.
#[cfg(target_os = "linux")]
fn guide_button(url: &'static str) -> impl IntoElement {
    control_button("perm-input-device-guide")
        .outline()
        .label(tr!("permissions.linux_input_access_guide"))
        .on_click(move |_, _, cx| cx.open_url(url))
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
            // open is refused — an exclusive open elsewhere (Logi
            // Options+, or Karabiner-Elements seizing a Bluetooth
            // keyboard, #2171), or a TCC session only a re-login
            // refreshes (#704).
            let stalled = status
                .as_ref()
                .is_some_and(|s| s.input_monitoring_granted && s.hid_open_failures);
            let badge = match status {
                Some(s) if s.input_monitoring_granted => PermissionStatus::Granted,
                Some(_) => PermissionStatus::Denied,
                None => PermissionStatus::Unknown,
            };
            let denied = badge == PermissionStatus::Denied;
            let field = gpui_kit::component::v_flex()
                .w_full()
                .gap_1()
                .child(permission_field(
                    "perm-input-monitoring",
                    badge,
                    Permission::InputMonitoring,
                    cx,
                ));
            let pal = theme::palette(cx);
            let hint = if stalled {
                Some(tr!("permissions.input_monitoring_granted_but_unavailable"))
            } else if denied {
                // The background agent reads the devices, so the grant has to
                // name it; one given to OpenLogi itself or to Terminal does
                // not reach it (#1851). Denied also covers an enabled entry
                // the agent cannot use: a stale one, or a grant made after
                // launch whose automatic relaunch was refused
                // (`InputMonitoring::NeedsRelaunch`). The hint covers both.
                Some(tr!("permissions.input_monitoring_agent_instructions"))
            } else {
                None
            };
            if let Some(text) = hint {
                field.child(div().text_caption().text_color(pal.text_muted).child(text))
            } else {
                field
            }
        }),
    )
    .description(tr!("permissions.input_monitoring_permission_description"))
    // Vertical: either hint runs to several sentences, which the right-hand
    // slot would clip at the card edge; stacked below the title it wraps.
    .layout(Axis::Vertical)
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
    // "Not determined" means never requested — Bluetooth deliberately never is
    // (BLE mice go through IOHIDManager) — so don't label it "Unknown".
    let never_requested = matches!(status, PermissionStatus::Unknown)
        && matches!(permission, Permission::Bluetooth | Permission::Camera);
    let status_el = if never_requested {
        badge(tr!("permissions.not_requested"), theme::STATUS_OFFLINE, pal)
    } else {
        status_badge(status, pal)
    };
    let prompts_here = never_requested && matches!(permission, Permission::Camera);
    let action_label = if prompts_here {
        tr!("permissions.grant")
    } else {
        tr!("common.open")
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
                .focus_visible(move |s| s.focus_ring(pal))
                .child(action_label)
                .on_click(move |_, _, cx| {
                    // Accessibility must be prompted in the agent (it owns the
                    // hook); prompting in the GUI would authorize the wrong
                    // binary. Other panes just deep-link to System Settings.
                    if matches!(permission, Permission::Accessibility)
                        && let Some(state) = crate::state::AppState::try_global(cx)
                    {
                        state.read(cx).request_accessibility_prompt();
                    }
                    // The Camera pane only lists an app after its first
                    // AVFoundation request, so a deep link can't grant it.
                    if prompts_here {
                        crate::features::camera::request_camera_access(cx);
                        return;
                    }
                    permissions::open_pane(permission);
                }),
        )
}
