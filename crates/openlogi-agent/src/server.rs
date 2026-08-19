//! tarpc IPC server: backs the [`Agent`] service with the orchestrator and the
//! agent-core device-I/O helpers, listening on the agent's local IPC socket
//! (a Unix-domain socket on Unix, a named pipe on Windows).
//!
//! The agent owns all device I/O, so the GUI never opens a device — it routes
//! "apply now" / "read" commands here, and polls snapshots.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use openlogi_agent_core::action_ring::ActionRingManager;
use openlogi_agent_core::event_monitor::SharedEventMonitor;
use openlogi_agent_core::hardware;
use openlogi_agent_core::hook_runtime::ActionDispatcher;
use openlogi_agent_core::observable::ObservableState;
use openlogi_agent_core::orchestrator::{Orchestrator, SharedRuntime};
use openlogi_core::binding::ActionRingSlot;
use openlogi_core::config::{Config, Lighting};
use openlogi_core::device::DeviceInventory;
use openlogi_hid::{
    DeviceRoute, DpiInfo, HapticWaveform, HidppOperation, LightCommand, ReceiverSelector,
    SmartShiftMode, SmartShiftStatus, WriteError,
};
use openlogi_ipc::transport;
use openlogi_ipc::{
    ActionRingCommandError, ActionRingInvocation, Agent, AgentSnapshot, AgentStatus,
    ConfigReloadError, Generation, Identity, MonitorEvent, Observation, PROTOCOL_VERSION,
    PairingCommandError, PairingUpdate,
};
use succession::Compat;

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
    /// Everything the GUI observes, answered from here rather than recomposed
    /// per request: reads take no orchestrator lock and no permission syscall.
    pub observable: Arc<ObservableState>,
    pub pairing: Arc<PairingManager>,
    pub event_monitor: SharedEventMonitor,
    pub action_ring: Arc<ActionRingManager>,
    pub dispatcher: ActionDispatcher,
    pub ring_haptics: RingHapticPlayer,
}

impl AgentServer {
    /// Build a server and start the coalescing Actions Ring haptic worker.
    pub fn new(
        orchestrator: Arc<Mutex<Orchestrator>>,
        shared: SharedRuntime,
        observable: Arc<ObservableState>,
        pairing: Arc<PairingManager>,
        event_monitor: SharedEventMonitor,
        action_ring: Arc<ActionRingManager>,
        dispatcher: ActionDispatcher,
    ) -> Self {
        let ring_haptics = RingHapticPlayer::spawn(shared.clone());
        Self {
            orchestrator,
            shared,
            observable,
            pairing,
            event_monitor,
            action_ring,
            dispatcher,
            ring_haptics,
        }
    }
}

impl Agent for AgentServer {
    async fn protocol_version(self, _: Context) -> u32 {
        PROTOCOL_VERSION
    }

    /// `Run::mint` is process-global, so the overlay supervisor hands its
    /// child the very same run this answers with.
    async fn identity(self, _: Context) -> Identity {
        Identity::mine(Compat::from(PROTOCOL_VERSION))
    }

    async fn status(self, _: Context) -> AgentStatus {
        self.observable.read(|state| state.status.clone())
    }

    async fn inventory(self, _: Context) -> Vec<DeviceInventory> {
        self.observable.read(|state| state.inventory.clone())
    }

    async fn reload_config(self, _: Context) -> Result<(), ConfigReloadError> {
        match Config::load_or_default() {
            Ok(config) => {
                let launch_at_login = config.app_settings.launch_at_login;
                self.orchestrator.lock().await.reload_config(config);
                // The GUI's launch-at-login toggle reaches us through this
                // reload, so re-reconcile the autostart from the new config.
                crate::launch_agent::reconcile(launch_at_login);
                Ok(())
            }
            Err(error) => {
                warn!(error = %error, "reload_config: parse failed; keeping current config");
                Err(ConfigReloadError {
                    message: error.to_string(),
                })
            }
        }
    }

    async fn set_dpi(self, _: Context, route: DeviceRoute, dpi: u32) -> Result<(), WriteError> {
        let dpi = hardware::dpi_wire_value(dpi)?;
        self.shared
            .device(&route)
            .run(HidppOperation::WriteDpi, |c| async move {
                openlogi_hid::set_dpi_on(&c, dpi).await
            })
            .await
    }

    async fn set_lighting(
        self,
        _: Context,
        route: DeviceRoute,
        lighting: Lighting,
    ) -> Result<(), WriteError> {
        let (r, g, b) = hardware::lighting_rgb(&lighting);
        self.shared
            .device(&route)
            .run(HidppOperation::Lighting, |c| async move {
                openlogi_hid::set_keyboard_color_on(&c, r, g, b).await
            })
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
        self.shared
            .device(&route)
            .run(HidppOperation::WriteSmartShift, |c| async move {
                openlogi_hid::set_smartshift_on(&c, mode, auto_disengage, tunable_torque).await
            })
            .await
    }

    async fn read_dpi(self, _: Context, route: DeviceRoute) -> Result<DpiInfo, WriteError> {
        self.shared
            .device(&route)
            .run(HidppOperation::ReadDpiCapabilities, |c| async move {
                openlogi_hid::get_dpi_info_on(&c).await
            })
            .await
    }

    async fn read_smartshift(
        self,
        _: Context,
        route: DeviceRoute,
    ) -> Result<SmartShiftStatus, WriteError> {
        self.shared
            .device(&route)
            .run(HidppOperation::ReadSmartShift, |c| async move {
                openlogi_hid::get_smartshift_status_on(&c).await
            })
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
        self.observable.snapshot()
    }

    async fn observe(self, _: Context, since: Generation) -> Observation {
        self.observable.observe(since).await
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
    pending_arm: Arc<std::sync::Mutex<Option<DeviceRoute>>>,
}

/// How long every attempt at one buzz may take together.
///
/// The write path bounds a single call at its own 5 s, which is sized for a
/// user-visible setting write, not for feedback: three attempts of it outlast
/// the ring's own 15 s lifetime, so one hover on a lossy channel could consume
/// a whole session while every later interaction — the activation buzz
/// included — was dropped by this worker's latest-wins channel. Past a couple
/// of seconds a buzz is not feedback any more anyway.
const PLAY_BUDGET: Duration = Duration::from_secs(2);

/// How long the firmware arming check may take before the first buzz.
///
/// Arming is a repair: a play works without it on any device that never lost
/// the state, so it must not be the thing that makes a session feel dead. Two
/// attempts at the write path's budget put 10 s in front of the first buzz.
const ARM_BUDGET: Duration = Duration::from_secs(1);

/// Re-assert the firmware haptic engine before the session's first buzz,
/// within [`ARM_BUDGET`].
///
/// Best-effort by design: a device that never lost the state plays fine
/// without this, so a silent channel must cost the session a bounded delay
/// rather than its whole haptic budget.
async fn arm_firmware_haptics(shared: &SharedRuntime, route: &DeviceRoute) {
    let budget = Budget::starting_at(Instant::now(), ARM_BUDGET);
    for attempt in 1..=2u8 {
        let Some(remaining) = budget.remaining(Instant::now()) else {
            warn!("firmware haptic check ran out of time — leaving it to the buzz");
            return;
        };
        match tokio::time::timeout(
            remaining,
            shared
                .device(route)
                .run(HidppOperation::PlayHaptic, |c| async move {
                    openlogi_hid::ensure_haptics_armed_on(&shared.channel_registry, &c).await
                }),
        )
        .await
        {
            Ok(Ok(true)) => {
                info!("firmware haptics were disarmed — re-enabled");
                return;
            }
            Ok(Ok(false)) => return,
            Ok(Err(error)) => {
                if attempt == 2 {
                    warn!(%error, "could not verify firmware haptic state");
                }
            }
            // Out of budget mid-attempt; the loop head reports it and returns.
            Err(_elapsed) => {}
        }
    }
}

/// Play one waveform, retrying within [`PLAY_BUDGET`]. Returns whether it played.
///
/// A busy receiver drops HID++ replies under concurrent pointer traffic, so a
/// single attempt fails exactly when the user is actively hovering. Retrying
/// is worth it — but only until either the budget is gone or a newer request
/// supersedes this one, since a stale buzz has no value and this worker is
/// single-flight.
async fn play_within_budget(
    shared: &SharedRuntime,
    rx: &tokio::sync::watch::Receiver<Option<(DeviceRoute, HapticWaveform, &'static str)>>,
    route: &DeviceRoute,
    waveform: HapticWaveform,
    interaction: &'static str,
) -> bool {
    let budget = Budget::starting_at(Instant::now(), PLAY_BUDGET);
    for attempt in 1..=3u8 {
        let Some(remaining) = budget.remaining(Instant::now()) else {
            warn!(
                interaction,
                "ring haptic gave up — out of time to be feedback"
            );
            return false;
        };
        match tokio::time::timeout(
            remaining,
            shared
                .device(route)
                .run(HidppOperation::PlayHaptic, |c| async move {
                    openlogi_hid::play_haptic_on(&shared.channel_registry, &c, waveform).await
                }),
        )
        .await
        {
            Ok(Ok(())) => return true,
            Ok(Err(error)) => {
                let superseded = rx.has_changed().unwrap_or(true);
                if attempt == 3 || superseded {
                    warn!(%error, interaction, attempt, superseded, "Actions Ring haptic failed");
                    return false;
                }
            }
            // Out of budget mid-attempt; the loop head reports it and stops,
            // so a lost reply and a slow one stay distinguishable in the log.
            Err(_elapsed) => {}
        }
    }
    false
}

/// A wall-clock allowance shared by every attempt at one interaction.
///
/// The worker is single-flight, so time one buzz spends retrying is time the
/// next interaction spends waiting. Attempts therefore share a budget rather
/// than each getting the write path's full one.
#[derive(Clone, Copy)]
struct Budget {
    deadline: Instant,
}

impl Budget {
    fn starting_at(now: Instant, total: Duration) -> Self {
        Self {
            deadline: now + total,
        }
    }

    /// How long the next attempt may take, or `None` when nothing is left.
    ///
    /// A spent budget is deliberately `None` rather than `Some(ZERO)`:
    /// `timeout(ZERO, …)` elapses before the call it wraps can run, which
    /// would log a device failure that never happened.
    fn remaining(self, now: Instant) -> Option<Duration> {
        let left = self.deadline.saturating_duration_since(now);
        (!left.is_zero()).then_some(left)
    }
}

impl RingHapticPlayer {
    /// Spawn the single-flight worker. Must be called from a tokio runtime.
    pub fn spawn(shared: SharedRuntime) -> Self {
        let (tx, mut rx) = tokio::sync::watch::channel::<
            Option<(DeviceRoute, HapticWaveform, &'static str)>,
        >(None);
        let pending_arm = Arc::new(std::sync::Mutex::new(None::<DeviceRoute>));
        let worker_arm = Arc::clone(&pending_arm);
        tokio::spawn(async move {
            let mut consecutive_failures = 0u32;
            let mut cooldown_until: Option<Instant> = None;
            while rx.changed().await.is_ok() {
                // Firmware arming is sequenced through this worker so it is
                // guaranteed to complete before the session's first buzz —
                // spawning it separately let the first hover race (and lose
                // to) a disarmed haptic engine.
                let arm_route = worker_arm
                    .lock()
                    .ok()
                    .and_then(|mut pending| pending.take());
                if let Some(route) = arm_route {
                    // A new session starts the breaker over. Both counters are
                    // worker-lifetime state, so without this a session that
                    // ended two failures deep makes the next session's first
                    // failure trip the cool-down, and a cool-down opened at the
                    // tail of one ring silences the opening of the next.
                    consecutive_failures = 0;
                    cooldown_until = None;
                    arm_firmware_haptics(&shared, &route).await;
                }
                let request = rx.borrow_and_update().clone();
                let Some((route, waveform, interaction)) = request else {
                    continue;
                };
                // Circuit breaker: when replies keep getting lost, further
                // attempts only keep the channel saturated (and starve
                // unrelated writes). Skip haptics for a short cool-down so
                // the pipe drains; feedback resumes on the next buzz after.
                if let Some(until) = cooldown_until {
                    if Instant::now() < until {
                        continue;
                    }
                    cooldown_until = None;
                }
                if play_within_budget(&shared, &rx, &route, waveform, interaction).await {
                    consecutive_failures = 0;
                } else {
                    consecutive_failures += 1;
                    if consecutive_failures >= 3 {
                        cooldown_until = Some(Instant::now() + Duration::from_secs(4));
                        consecutive_failures = 0;
                        info!(
                            interaction,
                            "ring haptics cooling down after repeated HID++ loss"
                        );
                    }
                }
            }
        });
        Self { tx, pending_arm }
    }

    /// Queue a firmware arming check to run before the session's first buzz.
    /// The wake-up send also clears any stale not-yet-played waveform from a
    /// previous session so it cannot replay.
    pub fn arm(&self, route: Option<DeviceRoute>) {
        let Some(route) = route else {
            return;
        };
        if let Ok(mut pending) = self.pending_arm.lock() {
            *pending = Some(route);
        }
        let _ = self.tx.send(None);
    }

    /// Queue `waveform`, replacing any not-yet-played request.
    fn play(
        &self,
        route: Option<DeviceRoute>,
        waveform: HapticWaveform,
        interaction: &'static str,
    ) {
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

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is idiomatic in tests")]
mod tests {
    use super::{ARM_BUDGET, Budget, PLAY_BUDGET};
    use std::time::{Duration, Instant};

    /// Attempts share one allowance, so the second gets what the first left —
    /// not another full one, which is how three retries came to outlast the
    /// ring they were giving feedback for.
    #[test]
    fn a_partly_spent_budget_offers_only_the_remainder() {
        let start = Instant::now();
        let budget = Budget::starting_at(start, Duration::from_secs(2));

        let left = budget
            .remaining(start + Duration::from_millis(1500))
            .unwrap();

        assert_eq!(left, Duration::from_millis(500));
    }

    /// `timeout(ZERO, …)` elapses before the call it wraps can run, so a spent
    /// budget has to read as "stop", not as "you have no time".
    #[test]
    fn a_spent_budget_offers_nothing_rather_than_zero() {
        let start = Instant::now();
        let budget = Budget::starting_at(start, Duration::from_secs(2));

        assert_eq!(budget.remaining(start + Duration::from_secs(2)), None);
        assert_eq!(budget.remaining(start + Duration::from_secs(5)), None);
    }

    /// Both budgets exist to keep one interaction from eating the 15 s session
    /// the ring is open for, and arming must not outlast the buzz it precedes.
    #[test]
    fn the_budgets_stay_within_a_ring_session() {
        assert!(ARM_BUDGET < PLAY_BUDGET);
        assert!(ARM_BUDGET + PLAY_BUDGET < Duration::from_secs(15));
    }
}
