//! tarpc IPC server: backs the [`Agent`] service with the orchestrator and the
//! agent-core device-I/O helpers, listening on the agent's local IPC socket
//! (a Unix-domain socket on Unix, a named pipe on Windows).
//!
//! The agent owns all device I/O, so the GUI never opens a device — it routes
//! "apply now" / "read" commands here, and polls snapshots.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt as _;
use openlogi_agent_core::action_ring::ActionRingManager;
use openlogi_agent_core::event_monitor::SharedEventMonitor;
use openlogi_agent_core::hook_runtime::ActionDispatcher;
use openlogi_agent_core::ipc::{
    ActionRingCommandError, ActionRingInvocation, Agent, AgentSnapshot, AgentStatus, MonitorEvent,
    PROTOCOL_VERSION, PairingCommandError, PairingUpdate,
};
use openlogi_agent_core::orchestrator::{Orchestrator, SharedRuntime};
use openlogi_agent_core::{hardware, transport};
use openlogi_core::binding::ActionRingSlot;
use openlogi_core::config::{Config, Lighting};
use openlogi_core::device::DeviceInventory;
use openlogi_hid::{
    DeviceRoute, DpiInfo, HapticWaveform, LightCommand, ReceiverSelector, SmartShiftMode,
    SmartShiftStatus, WriteError,
};

use crate::pairing::PairingManager;
// Brings `Listener::accept` into scope for the concrete listener `transport::bind`
// returns; `as _` keeps it anonymous (method resolution only).
use interprocess::local_socket::traits::tokio::Listener as _;
use openlogi_hook::Hook;
use tarpc::context::Context;
use tarpc::server::{BaseChannel, Channel as _};
use tokio::sync::Mutex;
use tracing::{info, warn};

/// Shared handle to the agent's state, cloned per connection (and per request).
#[derive(Clone)]
pub struct AgentServer {
    pub orchestrator: Arc<Mutex<Orchestrator>>,
    pub shared: SharedRuntime,
    pub hook_installed: Arc<AtomicBool>,
    pub pairing: Arc<PairingManager>,
    pub event_monitor: SharedEventMonitor,
    pub action_ring: Arc<ActionRingManager>,
    pub dispatcher: ActionDispatcher,
    pub ring_haptics: RingHapticPlayer,
}

impl Agent for AgentServer {
    async fn protocol_version(self, _: Context) -> u32 {
        PROTOCOL_VERSION
    }

    async fn status(self, _: Context) -> AgentStatus {
        let (launch_at_login, inventory) = {
            let orch = self.orchestrator.lock().await;
            (orch.launch_at_login(), orch.inventory_health())
        };
        AgentStatus {
            accessibility_granted: Hook::has_accessibility(),
            hook_installed: self.hook_installed.load(Ordering::Relaxed),
            launch_at_login,
            inventory,
            protocol_version: PROTOCOL_VERSION,
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    async fn inventory(self, _: Context) -> Vec<DeviceInventory> {
        self.orchestrator.lock().await.inventory()
    }

    async fn reload_config(self, _: Context) {
        match Config::load_or_default() {
            Ok(config) => {
                let launch_at_login = config.app_settings.launch_at_login;
                self.orchestrator.lock().await.reload_config(config);
                // The GUI's launch-at-login toggle reaches us through this
                // reload, so re-reconcile the autostart from the new config.
                crate::launch_agent::reconcile(launch_at_login);
            }
            Err(e) => warn!(error = %e, "reload_config: parse failed; keeping current config"),
        }
    }

    async fn set_dpi(self, _: Context, route: DeviceRoute, dpi: u32) -> Result<(), WriteError> {
        hardware::apply_dpi(
            &self.shared.capture_channel,
            &self.shared.channel_registry,
            &self.shared.receiver_access,
            &route,
            dpi,
        )
        .await
    }

    async fn set_lighting(
        self,
        _: Context,
        route: DeviceRoute,
        lighting: Lighting,
    ) -> Result<(), WriteError> {
        hardware::apply_lighting(
            &self.shared.capture_channel,
            &self.shared.channel_registry,
            &self.shared.receiver_access,
            &route,
            &lighting,
        )
        .await
    }

    async fn set_smartshift(
        self,
        _: Context,
        route: DeviceRoute,
        mode: SmartShiftMode,
        auto_disengage: u8,
        tunable_torque: u8,
    ) -> Result<(), WriteError> {
        hardware::apply_smartshift(
            &self.shared.capture_channel,
            &self.shared.channel_registry,
            &self.shared.receiver_access,
            &route,
            mode,
            auto_disengage,
            tunable_torque,
        )
        .await
    }

    async fn read_dpi(self, _: Context, route: DeviceRoute) -> Result<DpiInfo, WriteError> {
        hardware::read_dpi(
            &self.shared.capture_channel,
            &self.shared.channel_registry,
            &self.shared.receiver_access,
            &route,
        )
        .await
    }

    async fn read_smartshift(
        self,
        _: Context,
        route: DeviceRoute,
    ) -> Result<SmartShiftStatus, WriteError> {
        hardware::read_smartshift(
            &self.shared.capture_channel,
            &self.shared.channel_registry,
            &self.shared.receiver_access,
            &route,
        )
        .await
    }

    async fn request_accessibility_prompt(self, _: Context) {
        Hook::prompt_accessibility();
    }

    async fn start_pairing(
        self,
        _: Context,
        selector: ReceiverSelector,
    ) -> Result<(), PairingCommandError> {
        self.pairing.start(selector).await
    }

    async fn pair_device(self, _: Context, address: [u8; 6]) -> Result<(), PairingCommandError> {
        self.pairing.pair(address)
    }

    async fn cancel_pairing(self, _: Context) -> Result<(), PairingCommandError> {
        self.pairing.cancel()
    }

    async fn next_pairing(self, _: Context) -> Option<PairingUpdate> {
        self.pairing.next_update().await
    }

    async fn snapshot(self, _: Context) -> AgentSnapshot {
        let (launch_at_login, inventory_health, inventory, standalone, camera_active) = {
            let orch = self.orchestrator.lock().await;
            (
                orch.launch_at_login(),
                orch.inventory_health(),
                orch.inventory(),
                orch.standalone(),
                orch.camera_active(),
            )
        };
        AgentSnapshot {
            status: AgentStatus {
                accessibility_granted: Hook::has_accessibility(),
                hook_installed: self.hook_installed.load(Ordering::Relaxed),
                launch_at_login,
                inventory: inventory_health,
                protocol_version: PROTOCOL_VERSION,
                agent_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            inventory,
            standalone,
            camera_active,
        }
    }

    async fn poll_event_monitor(self, _: Context) -> Vec<MonitorEvent> {
        self.event_monitor.poll()
    }

    async fn set_light(
        self,
        _: Context,
        route: DeviceRoute,
        command: LightCommand,
    ) -> Result<(), WriteError> {
        hardware::cancel_light_reapply(&route);
        hardware::apply_light(&route, command).await
    }

    async fn set_light_manual_power(
        self,
        _: Context,
        route: DeviceRoute,
        enabled: bool,
    ) -> Result<(), WriteError> {
        hardware::cancel_light_reapply(&route);
        hardware::apply_light(&route, LightCommand::Power(enabled)).await?;
        if !self
            .orchestrator
            .lock()
            .await
            .set_manual_light_power(&route, enabled)
        {
            warn!(?route, "manual light power applied without camera override");
        }
        Ok(())
    }

    async fn next_action_ring(self, _: Context) -> Option<ActionRingInvocation> {
        self.action_ring.next_invocation().await
    }

    async fn action_ring_hover(
        self,
        _: Context,
        session_id: u64,
        slot: ActionRingSlot,
    ) -> Result<(), ActionRingCommandError> {
        if let Some(hover) = self.action_ring.hover(session_id, slot)? {
            self.ring_haptics
                .play(hover.haptic_route, HapticWaveform::SubtleCollision, "hover");
        }
        Ok(())
    }

    async fn action_ring_activate(
        self,
        _: Context,
        session_id: u64,
        slot: ActionRingSlot,
    ) -> Result<(), ActionRingCommandError> {
        let activation = self.action_ring.activate(session_id, slot)?;
        self.dispatcher
            .dispatch(&activation.action, Some(&activation.device_key));
        self.ring_haptics.play(
            activation.haptic_route,
            HapticWaveform::DampStateChange,
            "activation",
        );
        Ok(())
    }

    async fn action_ring_cancel(self, _: Context, session_id: u64) {
        self.action_ring.cancel(session_id);
    }
}

/// Coalescing Actions Ring haptic player: at most one waveform is in flight,
/// and while it plays only the newest queued request survives (latest wins).
///
/// Spawning one task per hover let a fast pointer queue HID++ plays faster
/// than the receiver drains them; the backlog then times out every
/// transaction on the channel for seconds at a time — dead haptics, and
/// collateral timeouts for unrelated writes (DPI, SmartShift) on the same
/// receiver. A stale hover buzz has no value, so dropping superseded requests
/// is strictly better than queueing them.
#[derive(Clone)]
pub struct RingHapticPlayer {
    tx: tokio::sync::watch::Sender<Option<(DeviceRoute, HapticWaveform, &'static str)>>,
}

impl RingHapticPlayer {
    /// Spawn the single-flight worker. Must be called from a tokio runtime.
    pub fn spawn(shared: SharedRuntime) -> Self {
        let (tx, mut rx) =
            tokio::sync::watch::channel::<Option<(DeviceRoute, HapticWaveform, &'static str)>>(
                None,
            );
        tokio::spawn(async move {
            while rx.changed().await.is_ok() {
                let request = rx.borrow_and_update().clone();
                let Some((route, waveform, interaction)) = request else {
                    continue;
                };
                if let Err(error) = hardware::play_haptic(
                    &shared.capture_channel,
                    &shared.channel_registry,
                    &shared.receiver_access,
                    &route,
                    waveform,
                )
                .await
                {
                    warn!(%error, interaction, "Actions Ring haptic failed");
                }
            }
        });
        Self { tx }
    }

    /// Queue `waveform`, replacing any not-yet-played request.
    fn play(&self, route: Option<DeviceRoute>, waveform: HapticWaveform, interaction: &'static str) {
        let Some(route) = route else {
            return;
        };
        let _ = self.tx.send(Some((route, waveform, interaction)));
    }
}

/// Bind the agent's IPC socket and serve [`Agent`] requests until the process
/// exits. A stale socket left by a prior crash is reclaimed by the listener —
/// `main` holds the single-instance lock (`agent.lock`), so no other live agent
/// owns this socket and any leftover is from a dead instance.
pub async fn run(server: AgentServer) {
    let listener = match transport::bind() {
        Ok(listener) => listener,
        Err(e) => {
            warn!(error = %e, "could not bind IPC socket; IPC disabled");
            return;
        }
    };
    info!("IPC server listening");

    loop {
        let stream = match listener.accept().await {
            Ok(stream) => stream,
            Err(e) => {
                warn!(error = %e, "IPC accept failed");
                continue;
            }
        };
        let server = server.clone();
        let channel = BaseChannel::with_defaults(transport::wrap(stream));
        tokio::spawn(
            channel
                .execute(server.serve())
                .for_each(|response| async move {
                    tokio::spawn(response);
                }),
        );
    }
}
