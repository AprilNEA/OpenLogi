//! Agent connection status and debug monitor state.

use super::{AgentLink, AppState};

impl AppState {
    /// Append a batch of live-monitor events, capping the retained history so the
    /// buffer can't grow without bound while the monitor is open.
    #[cfg(all(target_os = "macos", debug_assertions))]
    pub fn push_monitor_events(&mut self, events: Vec<openlogi_ipc::MonitorEvent>) {
        const MAX: usize = 200;
        self.monitor_events.extend(events);
        let overflow = self.monitor_events.len().saturating_sub(MAX);
        self.monitor_events.drain(..overflow);
    }
    /// Recent live-monitor events, oldest first.
    #[cfg(all(target_os = "macos", debug_assertions))]
    #[must_use]
    pub fn monitor_events(&self) -> &std::collections::VecDeque<openlogi_ipc::MonitorEvent> {
        &self.monitor_events
    }
    /// Replace the cached event-tap snapshot the Diagnostics page renders.
    /// Refreshed on the live-monitor poll tick; see [`Self::event_taps`].
    #[cfg(all(target_os = "macos", debug_assertions))]
    pub fn set_event_taps(&mut self, taps: Vec<openlogi_hook::EventTapInfo>) {
        self.event_taps = taps;
    }
    /// The cached event-tap snapshot for the Diagnostics page.
    #[cfg(all(target_os = "macos", debug_assertions))]
    #[must_use]
    pub fn event_taps(&self) -> &[openlogi_hook::EventTapInfo] {
        &self.event_taps
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
        &self.agent_link
    }
    /// The latest agent status snapshot — `None` while not connected (any
    /// non-[`AgentLink::Ready`] state), which readers like the Settings
    /// permission rows surface as "unknown", not "denied".
    #[must_use]
    pub fn agent_status(&self) -> Option<&openlogi_ipc::AgentStatus> {
        match &self.agent_link {
            AgentLink::Ready(status) => Some(status),
            _ => None,
        }
    }
    /// Replace the link, reporting whether it actually changed — the steady
    /// IPC poll mostly delivers identical snapshots, and the caller skips the
    /// window refresh for those.
    pub fn set_agent_link(&mut self, link: AgentLink) -> bool {
        if self.agent_link == link {
            return false;
        }
        self.agent_link = link;
        true
    }
}
