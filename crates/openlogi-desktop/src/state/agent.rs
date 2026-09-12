//! Agent connection status, snapshot projection, and debug monitor state.

use openlogi_camera::Camera;
use openlogi_core::device::DeviceInventory;
use openlogi_ipc::{AgentSnapshot, ForegroundApps, InventoryHealth};

use super::{AgentLink, AppState, StateEvent};
use crate::services::assets::AssetResolver;

/// State transitions produced by applying one complete agent snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnapshotChanges {
    pub(crate) inventory_ready: bool,
    pub(crate) events: Vec<StateEvent>,
}

impl SnapshotChanges {
    pub(crate) fn inventory_changed(&self) -> bool {
        self.events.contains(&StateEvent::InventoryChanged)
    }
}

/// Sheets already raised in this GUI session for agent-owned permissions.
#[cfg(any(test, target_os = "macos"))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PermissionPromptFlags {
    accessibility: bool,
    input_monitoring: bool,
    bluetooth: bool,
}

/// One agent-owned permission the GUI should ask for next.
#[cfg(any(test, target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentPermissionPrompt {
    Accessibility,
    InputMonitoring,
    Bluetooth,
}

/// Decide which missing agent permissions to prompt for on this Ready snapshot.
///
/// Input Monitoring Allow relaunches the agent. A Bluetooth request queued
/// behind that sheet is dropped, and a fast successor reconnect never leaves
/// Ready long enough to reset a one-shot flag. So Bluetooth is only asked
/// once Input Monitoring is already granted; the next Ready continues.
#[cfg(any(test, target_os = "macos"))]
fn next_agent_permission_prompts(
    status: &openlogi_ipc::AgentStatus,
    already: PermissionPromptFlags,
) -> (PermissionPromptFlags, Vec<AgentPermissionPrompt>) {
    let mut already = already;
    let mut prompts = Vec::new();
    if !status.accessibility_granted && !already.accessibility {
        already.accessibility = true;
        prompts.push(AgentPermissionPrompt::Accessibility);
    }
    if !status.input_monitoring_granted {
        if !already.input_monitoring {
            already.input_monitoring = true;
            prompts.push(AgentPermissionPrompt::InputMonitoring);
        }
        return (already, prompts);
    }
    if !status.bluetooth_granted && !already.bluetooth {
        already.bluetooth = true;
        prompts.push(AgentPermissionPrompt::Bluetooth);
    }
    (already, prompts)
}

/// Agent-owned observations accepted by the GUI for this process session.
pub(super) struct AgentSession {
    link: AgentLink,
    foreground: ForegroundApps,
    last_ready_inventory: Vec<DeviceInventory>,
    /// Which agent-owned permission sheets this GUI session has already
    /// asked for. Reset on any non-Ready link so a successor agent can
    /// finish what an Input Monitoring relaunch dropped.
    #[cfg(target_os = "macos")]
    permission_prompts: PermissionPromptFlags,
    #[cfg(all(target_os = "macos", debug_assertions))]
    monitor_events: std::collections::VecDeque<openlogi_ipc::MonitorEvent>,
    #[cfg(all(target_os = "macos", debug_assertions))]
    event_taps: Vec<openlogi_hook::EventTapInfo>,
}

impl Default for AgentSession {
    fn default() -> Self {
        Self {
            link: AgentLink::Connecting,
            foreground: ForegroundApps::default(),
            last_ready_inventory: Vec::new(),
            #[cfg(target_os = "macos")]
            permission_prompts: PermissionPromptFlags::default(),
            #[cfg(all(target_os = "macos", debug_assertions))]
            monitor_events: std::collections::VecDeque::new(),
            #[cfg(all(target_os = "macos", debug_assertions))]
            event_taps: Vec::new(),
        }
    }
}

impl AppState {
    /// Apply one complete agent snapshot through the desktop's production
    /// state projection, without requiring GPUI or an IPC connection.
    ///
    /// Pairing UI, emitted GPUI events, device-read scheduling, and asset sync
    /// remain runtime effects owned by the caller. This method owns only the
    /// durable snapshot-to-state merge shared by runtime delivery and tests.
    pub(crate) fn apply_agent_snapshot(
        &mut self,
        snapshot: &AgentSnapshot,
        cache: &AssetResolver,
        cameras: &[Camera],
    ) -> SnapshotChanges {
        let inventory_ready = snapshot.status.inventory == InventoryHealth::Ready;
        // Merge only completed enumerations. A scanning agent serves an empty
        // pre-enumeration list, which must not burn the GUI's miss grace or
        // replace the last known device set.
        let inventory = inventory_ready
            && self.refresh_inventories(&snapshot.inventory, &snapshot.standalone, cache, cameras);
        if inventory_ready {
            self.store_inventory_snapshot(&snapshot.inventory);
        }

        let agent = self.set_agent_link(AgentLink::Ready(snapshot.status.clone()));
        let camera = self.set_camera_active(snapshot.camera_active);
        let foreground = self.set_foreground(snapshot.foreground.clone());
        let mut events = Vec::new();
        if inventory {
            events.push(StateEvent::InventoryChanged);
        }
        if agent {
            events.push(StateEvent::AgentChanged);
        }
        if camera {
            events.push(StateEvent::CameraChanged);
        }
        if foreground {
            events.push(StateEvent::ForegroundChanged);
        }

        SnapshotChanges {
            inventory_ready,
            events,
        }
    }

    /// Append a batch of live-monitor events, capping the retained history so the
    /// buffer can't grow without bound while the monitor is open.
    #[cfg(all(target_os = "macos", debug_assertions))]
    pub fn push_monitor_events(&mut self, events: Vec<openlogi_ipc::MonitorEvent>) {
        const MAX: usize = 200;
        self.agent.monitor_events.extend(events);
        let overflow = self.agent.monitor_events.len().saturating_sub(MAX);
        self.agent.monitor_events.drain(..overflow);
    }
    /// Recent live-monitor events, oldest first.
    #[cfg(all(target_os = "macos", debug_assertions))]
    #[must_use]
    pub fn monitor_events(&self) -> &std::collections::VecDeque<openlogi_ipc::MonitorEvent> {
        &self.agent.monitor_events
    }
    /// Replace the cached event-tap snapshot the Diagnostics page renders.
    /// Refreshed on the live-monitor poll tick; see [`Self::event_taps`].
    #[cfg(all(target_os = "macos", debug_assertions))]
    pub fn set_event_taps(&mut self, taps: Vec<openlogi_hook::EventTapInfo>) {
        self.agent.event_taps = taps;
    }
    /// The cached event-tap snapshot for the Diagnostics page.
    #[cfg(all(target_os = "macos", debug_assertions))]
    #[must_use]
    pub fn event_taps(&self) -> &[openlogi_hook::EventTapInfo] {
        &self.agent.event_taps
    }
    /// Ask the agent to fire the macOS Accessibility prompt. The agent owns the
    /// CGEventTap, so the system dialog must name and authorize the *agent*
    /// binary; prompting in the GUI process (as the pre-split build did) would
    /// grant the wrong binary and the hook would never install.
    pub fn request_accessibility_prompt(&self, fallback_to_pane: bool) {
        self.send_ipc(crate::services::ipc::Command::RequestAccessibilityPrompt {
            fallback_to_pane,
        });
    }

    /// Ask the agent to fire the macOS Input Monitoring prompt.
    #[cfg(target_os = "macos")]
    pub fn request_input_monitoring_prompt(&self, fallback_to_pane: bool) {
        self.send_ipc(
            crate::services::ipc::Command::RequestInputMonitoringPrompt { fallback_to_pane },
        );
    }

    /// Ask the agent to fire the macOS Bluetooth prompt.
    #[cfg(target_os = "macos")]
    pub fn request_bluetooth_prompt(&self, fallback_to_pane: bool) {
        self.send_ipc(crate::services::ipc::Command::RequestBluetoothPrompt { fallback_to_pane });
    }

    /// After a Ready snapshot, ask the agent to raise native sheets for any
    /// permission it does not yet hold. Does not open System Settings.
    ///
    /// Bluetooth waits until Input Monitoring is already granted: an Allow on
    /// that sheet relaunches the agent and would drop a Bluetooth RPC queued
    /// behind it. The successor Ready then continues with whatever is still
    /// missing.
    #[cfg(target_os = "macos")]
    pub(crate) fn start_missing_agent_permission_prompts(
        &mut self,
        status: &openlogi_ipc::AgentStatus,
    ) {
        let (next, prompts) = next_agent_permission_prompts(status, self.agent.permission_prompts);
        self.agent.permission_prompts = next;
        for prompt in prompts {
            match prompt {
                AgentPermissionPrompt::Accessibility => {
                    self.request_accessibility_prompt(false);
                }
                AgentPermissionPrompt::InputMonitoring => {
                    self.request_input_monitoring_prompt(false);
                }
                AgentPermissionPrompt::Bluetooth => {
                    self.request_bluetooth_prompt(false);
                }
            }
        }
    }
    /// The agent connection state the render path branches on.
    #[must_use]
    pub fn agent_link(&self) -> &AgentLink {
        &self.agent.link
    }
    /// The latest agent status snapshot — `None` while not connected (any
    /// non-[`AgentLink::Ready`] state), which readers like the Settings
    /// permission rows surface as "unknown", not "denied".
    #[must_use]
    pub fn agent_status(&self) -> Option<&openlogi_ipc::AgentStatus> {
        match &self.agent.link {
            AgentLink::Ready(status) => Some(status),
            _ => None,
        }
    }
    /// Replace the link, reporting whether it actually changed — the steady
    /// IPC poll mostly delivers identical snapshots, and the caller skips the
    /// window refresh for those.
    pub fn set_agent_link(&mut self, link: AgentLink) -> bool {
        if self.agent.link == link {
            return false;
        }
        #[cfg(target_os = "macos")]
        if !matches!(link, AgentLink::Ready(_)) {
            self.agent.permission_prompts = PermissionPromptFlags::default();
        }
        self.agent.link = link;
        true
    }

    /// Cache a completed inventory snapshot for diagnostics.
    pub fn store_inventory_snapshot(&mut self, inventory: &[DeviceInventory]) {
        self.agent.last_ready_inventory = inventory.to_vec();
    }

    /// The last completed inventory snapshot, used by diagnostics.
    #[must_use]
    pub fn last_inventory(&self) -> &[DeviceInventory] {
        &self.agent.last_ready_inventory
    }

    /// Adopt the agent's foreground application snapshot.
    pub fn set_foreground(&mut self, foreground: ForegroundApps) -> bool {
        if self.agent.foreground == foreground {
            return false;
        }
        self.agent.foreground = foreground;
        true
    }

    pub(super) fn foreground(&self) -> &ForegroundApps {
        &self.agent.foreground
    }
}

#[cfg(test)]
mod tests {
    use openlogi_ipc::{AgentStatus, InventoryHealth, PROTOCOL_VERSION};

    use super::{AgentPermissionPrompt, PermissionPromptFlags, next_agent_permission_prompts};

    fn status(accessibility: bool, input_monitoring: bool, bluetooth: bool) -> AgentStatus {
        AgentStatus {
            accessibility_granted: accessibility,
            hook_installed: false,
            launch_at_login: false,
            inventory: InventoryHealth::Scanning,
            protocol_version: PROTOCOL_VERSION,
            agent_version: String::new(),
            input_monitoring_granted: input_monitoring,
            hid_open_failures: false,
            bluetooth_granted: bluetooth,
        }
    }

    #[test]
    fn first_ready_asks_accessibility_and_input_monitoring_but_not_bluetooth() {
        let (flags, prompts) = next_agent_permission_prompts(
            &status(false, false, false),
            PermissionPromptFlags::default(),
        );
        assert_eq!(
            prompts,
            [
                AgentPermissionPrompt::Accessibility,
                AgentPermissionPrompt::InputMonitoring
            ]
        );
        assert!(flags.accessibility);
        assert!(flags.input_monitoring);
        assert!(!flags.bluetooth);
    }

    #[test]
    fn input_monitoring_relaunch_ready_asks_only_bluetooth() {
        let already = PermissionPromptFlags {
            accessibility: true,
            input_monitoring: true,
            bluetooth: false,
        };
        let (flags, prompts) = next_agent_permission_prompts(&status(true, true, false), already);
        assert_eq!(prompts, [AgentPermissionPrompt::Bluetooth]);
        assert!(flags.bluetooth);
    }

    #[test]
    fn denied_input_monitoring_does_not_queue_bluetooth() {
        let already = PermissionPromptFlags {
            accessibility: true,
            input_monitoring: true,
            bluetooth: false,
        };
        let (flags, prompts) = next_agent_permission_prompts(&status(true, false, false), already);
        assert!(prompts.is_empty());
        assert!(!flags.bluetooth);
    }

    #[test]
    fn already_granted_input_monitoring_asks_bluetooth_immediately() {
        let (_, prompts) = next_agent_permission_prompts(
            &status(true, true, false),
            PermissionPromptFlags::default(),
        );
        assert_eq!(prompts, [AgentPermissionPrompt::Bluetooth]);
    }

    #[test]
    fn already_asked_bluetooth_is_not_asked_again() {
        let already = PermissionPromptFlags {
            accessibility: true,
            input_monitoring: true,
            bluetooth: true,
        };
        let (_, prompts) = next_agent_permission_prompts(&status(true, true, false), already);
        assert!(prompts.is_empty());
    }
}
