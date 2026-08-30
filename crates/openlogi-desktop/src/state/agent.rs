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

/// Agent-owned observations accepted by the GUI for this process session.
pub(super) struct AgentSession {
    link: AgentLink,
    foreground: ForegroundApps,
    last_ready_inventory: Vec<DeviceInventory>,
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
    pub fn request_accessibility_prompt(&self) {
        self.send_ipc(crate::services::ipc::Command::RequestAccessibilityPrompt);
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
