//! The System Settings drag guide for the agent's privacy grants.
//!
//! Accessibility and Input Monitoring belong to the agent, and the agent's
//! bundle sits inside the app bundle, where a pane's `+` picker cannot browse.
//! So while the agent lacks one of them, opening its pane also floats
//! `gpui_tcc`'s panel over System Settings, holding the agent's bundle for the
//! user to drag into the list. The next agent status that reports the grant
//! closes the panel.

use std::path::Path;

use anyhow::Context as _;
use gpui::{App, Bounds, ClickEvent, Global, Pixels, Window, px, size};
use gpui_tcc::{Guide, HostApp, PrivacyPanel};
use openlogi_ipc::AgentStatus;
use openlogi_permissions::{self as permissions, Permission};
use tracing::warn;

use crate::state::AppState;
use crate::ui::theme::CONTROL_H;

/// The permission the presented guide asks for, so a later agent status can
/// tell when it arrived.
struct Guided(Option<Permission>);

impl Global for Guided {}

/// Open `permission`'s System Settings pane, with the drag guide flying out of
/// the control `origin` clicked while the agent lacks a grant it can be given
/// that way.
pub(crate) fn open(permission: Permission, origin: &ClickEvent, window: &Window, cx: &mut App) {
    let panel = match permission {
        Permission::Accessibility => PrivacyPanel::Accessibility,
        Permission::InputMonitoring => PrivacyPanel::InputMonitoring,
        // Neither list takes a dropped app: an app appears there only after
        // its own request.
        Permission::Bluetooth | Permission::Camera => return open_pane(permission, cx),
    };
    let granted = AppState::try_read(cx)
        .and_then(AppState::agent_status)
        .is_some_and(|status| agent_holds(permission, status));
    if granted {
        return open_pane(permission, cx);
    }
    match present(panel, origin, window, cx) {
        Ok(()) => cx.set_global(Guided(Some(permission))),
        Err(error) => {
            warn!(error = %format_args!("{error:#}"), "could not present the System Settings guide");
            open_pane(permission, cx);
        }
    }
}

/// Close the presented guide once `status` shows the agent holding the grant
/// it asks for.
pub(crate) fn dismiss_once_granted(status: &AgentStatus, cx: &mut App) {
    let guided = cx
        .try_global::<Guided>()
        .and_then(|Guided(permission)| *permission);
    if guided.is_some_and(|permission| agent_holds(permission, status)) {
        cx.set_global(Guided(None));
        gpui_tcc::dismiss(cx);
    }
}

fn present(
    panel: PrivacyPanel,
    origin: &ClickEvent,
    window: &Window,
    cx: &mut App,
) -> anyhow::Result<()> {
    let agent = agent_app()?;
    let instruction = tr!("permissions.drag_app_into_list", app => agent.display_name.as_str());
    Guide::new(panel)
        .host_app(agent)
        .instruction(instruction)
        .launch_from(launch_bounds(origin), window)
        .present(cx)?;
    Ok(())
}

/// The agent as a privacy list names it: the helper bundle when packaged, the
/// bare binary in a dev profile without helpers.
fn agent_app() -> anyhow::Result<HostApp> {
    let exe = crate::services::ipc::agent_binary_path().context("no agent beside this app")?;
    let path = openlogi_core::brand::helper_bundle_root(&exe)
        .map_or_else(|| exe.clone(), Path::to_path_buf);
    Ok(HostApp::from_path(path)?)
}

/// The pane change, without a guide over it; one left over from an earlier
/// request would name the wrong list.
fn open_pane(permission: Permission, cx: &mut App) {
    cx.set_global(Guided(None));
    gpui_tcc::dismiss(cx);
    permissions::open_pane(permission);
}

fn agent_holds(permission: Permission, status: &AgentStatus) -> bool {
    match permission {
        Permission::Accessibility => status.accessibility_granted,
        Permission::InputMonitoring => status.input_monitoring_granted,
        Permission::Bluetooth | Permission::Camera => false,
    }
}

/// Where the panel flies out of. A key press reports the control's bounds; a
/// pointer click only where it landed, so a control-sized square there stands
/// in for the control.
fn launch_bounds(origin: &ClickEvent) -> Bounds<Pixels> {
    match origin {
        ClickEvent::Keyboard(press) => press.bounds,
        _ => Bounds::centered_at(origin.position(), size(px(CONTROL_H), px(CONTROL_H))),
    }
}
