//! What the GUI asks of the agent: one type per request.
//!
//! A request knows two things about itself — how to make its RPC over a live
//! client, and how to deliver the outcome to whoever asked, including the
//! outcome "no agent could be reached". Both live in one `impl`, so the
//! unreachable answer cannot drift from the reachable one, and adding a
//! request is one type plus one line in [`Command`], not an edit to parallel
//! dispatch tables.
//!
//! Replies take the shape the asker can consume. Reads carry a `oneshot` back
//! to the task awaiting them. Standalone-light writes and pairing commands
//! report through the GUI update stream, because the state that acts on them
//! is synchronous and the runtime is the one place that turns an IPC reply
//! into a state event. Fire-and-forget writes only log a rejection: the next
//! snapshot shows what the device actually did.

use std::future::Future;

use openlogi_core::config::Lighting;
use openlogi_core::hid::{
    DeviceRoute, Dpi, DpiInfo, LightCommand, ReceiverSelector, SmartShiftStatus, WriteError,
};
use openlogi_ipc::{AgentClient, ConfigReloadError, PairingCommandError, PairingFailure};
use tarpc::client::RpcError;
use tarpc::context;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use super::GuiUpdate;

/// The GPUI-bound update stream a request may deliver through.
pub(super) type UpdateSender = mpsc::UnboundedSender<GuiUpdate>;

/// No agent could be reached for a request: the socket was down when it was
/// dequeued, or the connection dropped before the answer arrived.
pub(super) struct Unavailable;

/// The connection dropped while a request was in flight; the loop reconnects.
pub(super) struct LinkLost;

/// One request the GUI makes of the agent.
pub(super) trait Request: Send + 'static {
    /// What the agent answers.
    type Answer: Send;

    /// Make the RPC over a live client.
    fn call(
        &self,
        client: &AgentClient,
    ) -> impl Future<Output = Result<Self::Answer, RpcError>> + Send;

    /// Hand the outcome to whoever asked — the agent's answer, or that no
    /// agent could be reached. Consumes the request, so a reply channel it
    /// carries can be used.
    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender);
}

/// Run one request: over `client` when there is one, answered as unavailable
/// otherwise. `Err` means the transport dropped mid-call, which the loop treats
/// as a lost link; the request has already been told.
pub(super) async fn run<R: Request>(
    request: R,
    client: Option<&AgentClient>,
    updates: &UpdateSender,
) -> Result<(), LinkLost> {
    let Some(client) = client else {
        request.deliver(Err(Unavailable), updates);
        return Ok(());
    };
    match request.call(client).await {
        Ok(answer) => {
            request.deliver(Ok(answer), updates);
            Ok(())
        }
        Err(error) => {
            debug!(%error, "request lost in transit — reconnecting");
            request.deliver(Err(Unavailable), updates);
            Err(LinkLost)
        }
    }
}

/// Unwrap a device answer, standing in for it when the agent could not be
/// reached. Transient, not a permanent feature error: the agent is just
/// restarting, so a panel keeps retrying instead of latching "unsupported".
fn or_unavailable<T>(outcome: Result<Result<T, WriteError>, Unavailable>) -> Result<T, WriteError> {
    outcome.unwrap_or(Err(WriteError::AgentUnavailable))
}

/// A fire-and-forget "apply now": a device-side refusal is logged, not
/// surfaced. An unreachable agent is not even that — the next snapshot shows
/// the device as it is.
fn log_rejection(what: &str, outcome: Result<Result<(), WriteError>, Unavailable>) {
    if let Ok(Err(error)) = outcome {
        warn!(%error, what, "agent rejected device command");
    }
}

/// Apply a DPI value to a device now.
pub struct SetDpi {
    pub route: DeviceRoute,
    pub dpi: Dpi,
}

impl Request for SetDpi {
    type Answer = Result<(), WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .set_dpi(context::current(), self.route.clone(), self.dpi)
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, _: &UpdateSender) {
        log_rejection("DPI", outcome);
    }
}

/// Apply a keyboard lighting configuration now.
pub struct SetLighting {
    pub route: DeviceRoute,
    pub lighting: Lighting,
}

impl Request for SetLighting {
    type Answer = Result<(), WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .set_lighting(
                context::current(),
                self.route.clone(),
                self.lighting.clone(),
            )
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, _: &UpdateSender) {
        log_rejection("lighting", outcome);
    }
}

/// Apply a SmartShift configuration now.
pub struct SetSmartShift {
    pub route: DeviceRoute,
    pub status: SmartShiftStatus,
}

impl Request for SetSmartShift {
    type Answer = Result<(), WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .set_smartshift(context::current(), self.route.clone(), self.status)
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, _: &UpdateSender) {
        log_rejection("SmartShift", outcome);
    }
}

/// Report a standalone-light write to the state that made it. `key` and
/// `request_id` identify that edit, so a slow failure cannot overwrite the
/// status of a newer slider release.
fn report_light_result(
    updates: &UpdateSender,
    key: String,
    request_id: u64,
    command: LightCommand,
    outcome: Result<Result<(), WriteError>, Unavailable>,
) {
    let _ = updates.send(GuiUpdate::LightCommandResult {
        key,
        request_id,
        command,
        result: or_unavailable(outcome),
    });
}

/// One standalone-light control write, answered as a
/// [`GuiUpdate::LightCommandResult`] for the edit `key`/`request_id` names.
pub struct SetLight {
    pub route: DeviceRoute,
    pub command: LightCommand,
    pub key: String,
    pub request_id: u64,
}

impl Request for SetLight {
    type Answer = Result<(), WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .set_light(context::current(), self.route.clone(), self.command)
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        report_light_result(updates, self.key, self.request_id, self.command, outcome);
    }
}

/// Switch a standalone light's manual power, answered like [`SetLight`] under
/// [`LightCommand::Power`].
pub struct SetLightManualPower {
    pub route: DeviceRoute,
    pub enabled: bool,
    pub key: String,
    pub request_id: u64,
}

impl Request for SetLightManualPower {
    type Answer = Result<(), WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .set_light_manual_power(context::current(), self.route.clone(), self.enabled)
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        report_light_result(
            updates,
            self.key,
            self.request_id,
            LightCommand::Power(self.enabled),
            outcome,
        );
    }
}

/// Read a device's DPI state; the answer goes back over `reply`.
pub struct ReadDpi {
    pub route: DeviceRoute,
    pub reply: oneshot::Sender<Result<DpiInfo, WriteError>>,
}

impl Request for ReadDpi {
    type Answer = Result<DpiInfo, WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .read_dpi(context::current(), self.route.clone())
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, _: &UpdateSender) {
        let _ = self.reply.send(or_unavailable(outcome));
    }
}

/// Read a device's SmartShift state; the answer goes back over `reply`.
pub struct ReadSmartShift {
    pub route: DeviceRoute,
    pub reply: oneshot::Sender<Result<SmartShiftStatus, WriteError>>,
}

impl Request for ReadSmartShift {
    type Answer = Result<SmartShiftStatus, WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .read_smartshift(context::current(), self.route.clone())
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, _: &UpdateSender) {
        let _ = self.reply.send(or_unavailable(outcome));
    }
}

/// Have the agent re-read `config.toml`.
///
/// The loop holds this one until a connection exists and never answers it
/// locally (module doc), so `deliver` only ever reports the agent's own
/// verdict: a reload that did not happen is not one the agent refused.
pub struct ReloadConfig;

impl Request for ReloadConfig {
    type Answer = Result<(), ConfigReloadError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client.reload_config(context::current()).await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        if let Ok(verdict) = outcome {
            let _ = updates.send(GuiUpdate::ConfigReloadResult(verdict));
        }
    }
}

/// Ask the agent to fire the macOS Accessibility prompt. The agent owns the
/// CGEventTap, so the system dialog must name (and authorize) the *agent*
/// binary, not the GUI — prompting locally would grant the wrong process. An
/// unreachable agent cannot prompt, and the snapshot shows the permission
/// state either way.
pub struct RequestAccessibilityPrompt;

impl Request for RequestAccessibilityPrompt {
    type Answer = ();

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .request_accessibility_prompt(context::current())
            .await
    }

    fn deliver(self, _: Result<Self::Answer, Unavailable>, _: &UpdateSender) {}
}

/// An accepted pairing command needs no reply — its progress shows up in the
/// observed state. A *rejected* one never becomes a session, and neither does
/// one the agent never received, so the refusal is reported here or the
/// window would wait for something that will never come.
fn report_pairing_refusal(
    updates: &UpdateSender,
    outcome: Result<Result<(), PairingCommandError>, Unavailable>,
) {
    let failure = match outcome {
        Ok(Ok(())) => return,
        Ok(Err(refused)) => PairingFailure::from(refused),
        Err(Unavailable) => PairingFailure::AgentRestarted,
    };
    let _ = updates.send(GuiUpdate::PairingUndeliverable(failure));
}

/// Begin a pairing session on a receiver.
pub struct StartPairing {
    pub selector: ReceiverSelector,
}

impl Request for StartPairing {
    type Answer = Result<(), PairingCommandError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .start_pairing(context::current(), self.selector.clone())
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        report_pairing_refusal(updates, outcome);
    }
}

/// Pair a discovered device by address.
pub struct PairDevice {
    pub address: [u8; 6],
}

impl Request for PairDevice {
    type Answer = Result<(), PairingCommandError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client.pair_device(context::current(), self.address).await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        report_pairing_refusal(updates, outcome);
    }
}

/// End the pairing session. Nothing to report when the agent is unreachable:
/// there is no session left for the window to wait on, and the observed state
/// says so.
pub struct CancelPairing;

impl Request for CancelPairing {
    type Answer = Result<(), PairingCommandError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client.cancel_pairing(context::current()).await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        if let Ok(Err(refused)) = outcome {
            let _ = updates.send(GuiUpdate::PairingUndeliverable(PairingFailure::from(
                refused,
            )));
        }
    }
}

/// Drain the agent's live event-monitor buffer for the debug Diagnostics
/// monitor. The first poll enables monitoring agent-side; the agent
/// auto-disables it once polls stop. An unreachable agent has nothing to
/// drain.
#[cfg(all(target_os = "macos", debug_assertions))]
pub struct PollEventMonitor {
    pub reply: oneshot::Sender<Vec<openlogi_ipc::MonitorEvent>>,
}

#[cfg(all(target_os = "macos", debug_assertions))]
impl Request for PollEventMonitor {
    type Answer = Vec<openlogi_ipc::MonitorEvent>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client.poll_event_monitor(context::current()).await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, _: &UpdateSender) {
        let _ = self.reply.send(outcome.unwrap_or_default());
    }
}

/// The message from the GPUI thread to the client thread: one variant per
/// request type, so the state tests can see exactly what was sent. Every
/// request converts into it with `From`, and running it dispatches to the
/// request's own [`Request`] impl.
macro_rules! commands {
    ($($(#[$cfg:meta])* $variant:ident),* $(,)?) => {
        /// A request from the GPUI thread to the client thread. Reads carry a
        /// `oneshot` for the reply; light writes and pairing commands report
        /// through [`GuiUpdate`]; the rest are fire-and-forget.
        pub enum Command {
            $($(#[$cfg])* $variant($variant),)*
        }

        $($(#[$cfg])*
        impl From<$variant> for Command {
            fn from(request: $variant) -> Self {
                Self::$variant(request)
            }
        })*

        impl Command {
            /// Run this request over `client`, or answer it as unavailable.
            pub(super) async fn run(
                self,
                client: Option<&AgentClient>,
                updates: &UpdateSender,
            ) -> Result<(), LinkLost> {
                match self {
                    $($(#[$cfg])* Self::$variant(request) => run(request, client, updates).await,)*
                }
            }
        }
    };
}

commands! {
    SetDpi,
    SetLighting,
    SetLight,
    SetLightManualPower,
    SetSmartShift,
    ReadDpi,
    ReadSmartShift,
    ReloadConfig,
    RequestAccessibilityPrompt,
    StartPairing,
    PairDevice,
    CancelPairing,
    #[cfg(all(target_os = "macos", debug_assertions))]
    PollEventMonitor,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pairing_command_the_agent_never_received_fails_the_window_out_of_waiting() {
        let (updates, mut received) = mpsc::unbounded_channel();

        StartPairing {
            selector: ReceiverSelector::First,
        }
        .deliver(Err(Unavailable), &updates);

        assert!(matches!(
            received.try_recv(),
            Ok(GuiUpdate::PairingUndeliverable(
                PairingFailure::AgentRestarted
            ))
        ));
    }

    #[test]
    fn an_accepted_pairing_command_reports_nothing() {
        // Progress arrives in the observed state; a local "accepted" would
        // only race it.
        let (updates, mut received) = mpsc::unbounded_channel();

        PairDevice { address: [0; 6] }.deliver(Ok(Ok(())), &updates);

        assert!(received.try_recv().is_err());
    }

    #[test]
    fn a_cancel_that_found_no_agent_reports_nothing() {
        let (updates, mut received) = mpsc::unbounded_channel();

        CancelPairing.deliver(Err(Unavailable), &updates);

        assert!(received.try_recv().is_err());
    }

    #[test]
    fn a_reload_that_never_reached_the_agent_is_not_a_verdict() {
        let (updates, mut received) = mpsc::unbounded_channel();

        ReloadConfig.deliver(Err(Unavailable), &updates);

        assert!(received.try_recv().is_err());
    }
}
