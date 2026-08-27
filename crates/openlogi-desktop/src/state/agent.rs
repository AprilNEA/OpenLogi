//! Agent connection status and debug monitor state.

use openlogi_core::device::DeviceInventory;
use openlogi_ipc::{ForegroundApps, PrimaryMouseButton};

use crate::services::ipc::PrimaryMouseButtonCommandError;

use super::{AgentLink, AppState};

/// Agent-owned observations accepted by the GUI for this process session.
pub(super) struct AgentSession {
    link: AgentLink,
    foreground: ForegroundApps,
    primary_mouse_button: Option<PrimaryMouseButton>,
    primary_mouse_button_pending: Option<PrimaryMouseButton>,
    primary_mouse_button_error: Option<PrimaryMouseButtonCommandError>,
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
            primary_mouse_button: None,
            primary_mouse_button_pending: None,
            primary_mouse_button_error: None,
            last_ready_inventory: Vec::new(),
            #[cfg(all(target_os = "macos", debug_assertions))]
            monitor_events: std::collections::VecDeque::new(),
            #[cfg(all(target_os = "macos", debug_assertions))]
            event_taps: Vec::new(),
        }
    }
}

impl AppState {
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

    /// The latest host-wide primary mouse button reported by the agent.
    #[must_use]
    pub fn primary_mouse_button(&self) -> Option<PrimaryMouseButton> {
        self.agent.primary_mouse_button
    }

    /// Whether a host-wide primary-button write is waiting for the agent.
    #[must_use]
    pub fn primary_mouse_button_pending(&self) -> bool {
        self.agent.primary_mouse_button_pending.is_some()
    }

    /// The last primary-button write failure, retained until the user retries
    /// or the agent confirms a later write.
    #[must_use]
    pub fn primary_mouse_button_error(&self) -> Option<&PrimaryMouseButtonCommandError> {
        self.agent.primary_mouse_button_error.as_ref()
    }

    /// Adopt a host-wide primary mouse button observation.
    pub fn set_primary_mouse_button(&mut self, button: Option<PrimaryMouseButton>) -> bool {
        if self.agent.primary_mouse_button == button {
            return false;
        }
        self.agent.primary_mouse_button = button;
        true
    }

    /// Ask the agent to change the macOS system setting. The observed snapshot,
    /// not this pending request, remains the GUI's source of truth. Returns
    /// whether request/error presentation changed.
    pub fn request_primary_mouse_button(&mut self, button: PrimaryMouseButton) -> bool {
        let previous = (
            self.agent.primary_mouse_button_pending,
            self.agent.primary_mouse_button_error.clone(),
        );
        if self.send_ipc(crate::services::ipc::Command::SetPrimaryMouseButton(button)) {
            self.agent.primary_mouse_button_pending = Some(button);
            self.agent.primary_mouse_button_error = None;
        } else {
            self.agent.primary_mouse_button_pending = None;
            self.agent.primary_mouse_button_error =
                Some(PrimaryMouseButtonCommandError::AgentUnavailable);
        }
        previous
            != (
                self.agent.primary_mouse_button_pending,
                self.agent.primary_mouse_button_error.clone(),
            )
    }

    /// Record whether the agent accepted the latest primary-button write.
    /// Success only clears command presentation; the observed snapshot remains
    /// responsible for changing the switch's value.
    pub fn apply_primary_mouse_button_result(
        &mut self,
        result: Result<PrimaryMouseButton, PrimaryMouseButtonCommandError>,
    ) -> bool {
        let previous = (
            self.agent.primary_mouse_button_pending,
            self.agent.primary_mouse_button_error.clone(),
        );
        self.agent.primary_mouse_button_pending = None;
        self.agent.primary_mouse_button_error = result.err();
        previous
            != (
                self.agent.primary_mouse_button_pending,
                self.agent.primary_mouse_button_error.clone(),
            )
    }
}
