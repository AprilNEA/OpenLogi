//! Agent connection status, snapshot projection, and debug monitor state.

use openlogi_camera::Camera;
use openlogi_core::device::DeviceInventory;
use openlogi_ipc::PrimaryMouseButton;
use openlogi_ipc::{AgentSnapshot, ForegroundApps, InventoryHealth};

#[cfg(target_os = "macos")]
use crate::services::ipc::PrimaryMouseButtonCommandError;

use super::events::StateEvents;
use super::{AgentLink, AppState, StateEvent};
use crate::services::assets::AssetResolver;

/// State transitions produced by applying one complete agent snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnapshotChanges {
    pub(crate) inventory_ready: bool,
    pub(crate) events: StateEvents,
}

impl SnapshotChanges {
    pub(crate) fn inventory_changed(&self) -> bool {
        self.events.contains(&StateEvent::InventoryChanged)
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum PrimaryMouseButtonCommandState {
    #[default]
    Idle,
    Pending(PrimaryMouseButton),
    Failed(PrimaryMouseButtonCommandError),
    /// The agent may have applied this request before the RPC connection
    /// dropped. A matching authoritative snapshot resolves the ambiguity.
    AwaitingConfirmation(PrimaryMouseButton),
}

#[cfg(target_os = "macos")]
impl PrimaryMouseButtonCommandState {
    fn error(&self) -> Option<PrimaryMouseButtonCommandError> {
        match self {
            Self::Failed(error) => Some(error.clone()),
            Self::AwaitingConfirmation(_) => Some(PrimaryMouseButtonCommandError::AgentUnavailable),
            Self::Idle | Self::Pending(_) => None,
        }
    }

    fn reconcile_observation(&mut self, button: Option<PrimaryMouseButton>) -> bool {
        let Self::AwaitingConfirmation(requested) = self else {
            return false;
        };
        if button != Some(*requested) {
            return false;
        }
        *self = Self::Idle;
        true
    }
}

/// Agent-owned observations accepted by the GUI for this process session.
pub(super) struct AgentSession {
    link: AgentLink,
    foreground: ForegroundApps,
    primary_mouse_button: Option<PrimaryMouseButton>,
    #[cfg(target_os = "macos")]
    primary_mouse_button_command: PrimaryMouseButtonCommandState,
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
            #[cfg(target_os = "macos")]
            primary_mouse_button_command: PrimaryMouseButtonCommandState::default(),
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
        resolver: &AssetResolver,
        cameras: &[Camera],
    ) -> SnapshotChanges {
        let inventory_ready = snapshot.status.inventory == InventoryHealth::Ready;
        // Merge only completed enumerations. A scanning agent serves an empty
        // pre-enumeration list, which must not burn the GUI's miss grace or
        // replace the last known device set.
        let inventory = if inventory_ready {
            self.refresh_inventories(&snapshot.inventory, &snapshot.standalone, resolver, cameras)
        } else {
            StateEvents::none()
        };
        if inventory_ready {
            self.store_inventory_snapshot(&snapshot.inventory);
        }

        let agent = self.set_agent_link(AgentLink::Ready(snapshot.status.clone()));
        let camera = self.set_camera_active(snapshot.camera_active);
        let foreground = self.set_foreground(snapshot.foreground.clone());
        let primary_mouse_button = self.set_primary_mouse_button(snapshot.primary_mouse_button);

        SnapshotChanges {
            inventory_ready,
            events: inventory
                .and(agent)
                .and(camera)
                .and(foreground)
                .and(primary_mouse_button),
        }
    }

    /// Fold one live-monitor poll into what the Diagnostics page renders: the
    /// refreshed event-tap snapshot, and whatever events arrived since the
    /// last poll.
    #[cfg(all(target_os = "macos", debug_assertions))]
    pub fn record_monitor_poll(
        &mut self,
        taps: Vec<openlogi_hook::EventTapInfo>,
        events: Vec<openlogi_ipc::MonitorEvent>,
    ) -> StateEvents {
        self.set_event_taps(taps);
        if !events.is_empty() {
            self.push_monitor_events(events);
        }
        StateEvent::DiagnosticsChanged.into()
    }
    /// Append a batch of live-monitor events, capping the retained history so the
    /// buffer can't grow without bound while the monitor is open.
    #[cfg(all(target_os = "macos", debug_assertions))]
    fn push_monitor_events(&mut self, events: Vec<openlogi_ipc::MonitorEvent>) {
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
    fn set_event_taps(&mut self, taps: Vec<openlogi_hook::EventTapInfo>) {
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
        self.send_ipc(crate::services::ipc::RequestAccessibilityPrompt);
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
    /// Replace the link, reporting it only when it actually changed — most
    /// observed snapshots leave the link as it was, and those must not refresh
    /// the window.
    pub fn set_agent_link(&mut self, link: AgentLink) -> StateEvents {
        if self.agent.link == link {
            return StateEvents::none();
        }
        self.agent.link = link;
        StateEvent::AgentChanged.into()
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
    pub fn set_foreground(&mut self, foreground: ForegroundApps) -> StateEvents {
        if self.agent.foreground == foreground {
            return StateEvents::none();
        }
        self.agent.foreground = foreground;
        StateEvent::ForegroundChanged.into()
    }

    pub(super) fn foreground(&self) -> &ForegroundApps {
        &self.agent.foreground
    }

    /// The latest host-wide primary mouse button reported by the agent.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn primary_mouse_button(&self) -> Option<PrimaryMouseButton> {
        self.agent.primary_mouse_button
    }

    /// Whether a host-wide primary-button write is waiting for the agent.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn primary_mouse_button_pending(&self) -> bool {
        matches!(
            self.agent.primary_mouse_button_command,
            PrimaryMouseButtonCommandState::Pending(_)
        )
    }

    /// The last primary-button write failure, retained until the user retries
    /// or an authoritative snapshot confirms an indeterminate write.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn primary_mouse_button_error(&self) -> Option<PrimaryMouseButtonCommandError> {
        self.agent.primary_mouse_button_command.error()
    }

    /// Adopt a host-wide primary mouse button observation, using it to resolve
    /// a write whose RPC reply was lost after the agent may have applied it.
    pub fn set_primary_mouse_button(&mut self, button: Option<PrimaryMouseButton>) -> StateEvents {
        let button_changed = self.agent.primary_mouse_button != button;
        if button_changed {
            self.agent.primary_mouse_button = button;
        }
        #[cfg(target_os = "macos")]
        {
            let command_changed = self
                .agent
                .primary_mouse_button_command
                .reconcile_observation(button);
            (button_changed || command_changed)
                .then_some(StateEvent::SettingsChanged)
                .into()
        }
        #[cfg(not(target_os = "macos"))]
        {
            button_changed.then_some(StateEvent::SettingsChanged).into()
        }
    }

    /// Ask the agent to change the macOS system setting. The observed snapshot,
    /// not this pending request, remains the GUI's source of truth. Returns
    /// which event reports a changed request/error presentation.
    #[cfg(target_os = "macos")]
    pub fn request_primary_mouse_button(&mut self, button: PrimaryMouseButton) -> StateEvents {
        let previous = self.agent.primary_mouse_button_command.clone();
        self.agent.primary_mouse_button_command = if self
            .send_ipc(crate::services::ipc::SetPrimaryMouseButton { button })
        {
            PrimaryMouseButtonCommandState::Pending(button)
        } else {
            PrimaryMouseButtonCommandState::Failed(PrimaryMouseButtonCommandError::AgentUnavailable)
        };
        (previous != self.agent.primary_mouse_button_command)
            .then_some(StateEvent::SettingsChanged)
            .into()
    }

    /// Record whether the agent accepted the latest primary-button write. A
    /// lost reply keeps the requested value so an authoritative snapshot can
    /// still confirm it; the snapshot remains responsible for the switch value.
    #[cfg(target_os = "macos")]
    pub fn apply_primary_mouse_button_result(
        &mut self,
        result: Result<PrimaryMouseButton, PrimaryMouseButtonCommandError>,
    ) -> StateEvents {
        let previous = self.agent.primary_mouse_button_command.clone();
        self.agent.primary_mouse_button_command = match result {
            Ok(_) => PrimaryMouseButtonCommandState::Idle,
            Err(PrimaryMouseButtonCommandError::AgentUnavailable) => match &previous {
                PrimaryMouseButtonCommandState::Pending(requested)
                    if self.agent.primary_mouse_button == Some(*requested) =>
                {
                    PrimaryMouseButtonCommandState::Idle
                }
                PrimaryMouseButtonCommandState::Pending(requested) => {
                    PrimaryMouseButtonCommandState::AwaitingConfirmation(*requested)
                }
                _ => PrimaryMouseButtonCommandState::Failed(
                    PrimaryMouseButtonCommandError::AgentUnavailable,
                ),
            },
            Err(error) => PrimaryMouseButtonCommandState::Failed(error),
        };
        (previous != self.agent.primary_mouse_button_command)
            .then_some(StateEvent::SettingsChanged)
            .into()
    }
}
