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

use openlogi_core::config::{ConfigRevision, Lighting};
use openlogi_core::hid::{
    BacklightChange, BacklightState, DeviceRoute, DisableKeysMask, DisableKeysState, Dpi, DpiInfo,
    EasySwitchHosts, FnLockState, ForceSensingInfo, LightCommand, PointerSpeed, ReceiverSelector,
    ReportRate, ReportRateInfo, SmartShiftStatus, WriteError,
};
use openlogi_ipc::{
    AgentClient, ChosenLight, ConfigReloadError, PairingCommandError, PairingFailure, SavedConfig,
    SavedConfigVerdict,
};
use tarpc::client::RpcError;
use tarpc::context;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use super::{
    ConfigReloadContext, DisableKeysRequestContext, GuiUpdate, UnpairFailure, UnpairIntent,
};
use crate::state::DeviceKey;

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

/// Write a DPI value to a device now, answered as [`GuiUpdate::DpiWritten`]
/// with the device's verdict.
pub struct SetDpi {
    pub route: DeviceRoute,
    pub dpi: Dpi,
    pub key: DeviceKey,
    /// The number the state gave this write, returned with the answer so it
    /// is matched to the write it answers.
    pub write: u64,
}

impl Request for SetDpi {
    type Answer = Result<(), WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .set_dpi(context::current(), self.route.clone(), self.dpi)
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        let _ = updates.send(GuiUpdate::DpiWritten {
            key: self.key,
            write: self.write,
            dpi: self.dpi,
            result: or_unavailable(outcome),
        });
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

/// Write keyboard Fn-lock now, answered as [`GuiUpdate::FnLockWritten`] with
/// the state the keyboard echoes back.
pub struct SetFnLock {
    pub route: DeviceRoute,
    pub fn_lock: bool,
    pub key: DeviceKey,
}

impl Request for SetFnLock {
    type Answer = Result<FnLockState, WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .set_fn_lock(context::current(), self.route.clone(), self.fn_lock)
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        let _ = updates.send(GuiUpdate::FnLockWritten {
            key: self.key,
            result: or_unavailable(outcome),
        });
    }
}

/// Write a mouse's report rate now, answered as
/// [`GuiUpdate::ReportRateWritten`] with what the mouse reads back.
pub struct SetReportRate {
    pub route: DeviceRoute,
    pub rate: ReportRate,
    pub key: DeviceKey,
}

impl Request for SetReportRate {
    type Answer = Result<ReportRateInfo, WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .set_report_rate(context::current(), self.route.clone(), self.rate)
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        let _ = updates.send(GuiUpdate::ReportRateWritten {
            key: self.key,
            route: self.route,
            result: or_unavailable(outcome),
        });
    }
}

/// Write a mouse's pointer speed now, answered as
/// [`GuiUpdate::PointerSpeedWritten`] with what the mouse reads back.
pub struct SetPointerSpeed {
    pub route: DeviceRoute,
    pub speed: PointerSpeed,
    pub key: DeviceKey,
}

impl Request for SetPointerSpeed {
    type Answer = Result<PointerSpeed, WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .set_pointer_speed(context::current(), self.route.clone(), self.speed)
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        let _ = updates.send(GuiUpdate::PointerSpeedWritten {
            key: self.key,
            result: or_unavailable(outcome),
        });
    }
}

/// Write the press force of a force-sensing button now, answered as
/// [`GuiUpdate::ForceSensingWritten`] with what the device reads back.
pub struct SetForceSensing {
    pub route: DeviceRoute,
    pub force: u16,
    pub key: DeviceKey,
    /// The number the state gave this write, returned with the answer so it
    /// is matched to the write it answers.
    pub write: u64,
}

impl Request for SetForceSensing {
    type Answer = Result<ForceSensingInfo, WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .set_force_sensing(context::current(), self.route.clone(), self.force)
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        let _ = updates.send(GuiUpdate::ForceSensingWritten {
            key: self.key,
            write: self.write,
            force: self.force,
            result: or_unavailable(outcome),
        });
    }
}

/// Apply one backlight change to a keyboard now, answered as
/// [`GuiUpdate::BacklightWritten`] with what the keyboard reads back.
pub struct SetBacklight {
    pub route: DeviceRoute,
    pub change: BacklightChange,
    pub key: DeviceKey,
    /// The number the device-read service gave this change, returned with
    /// the answer so it is matched to the change it answers.
    pub write: u64,
}

impl Request for SetBacklight {
    type Answer = Result<BacklightState, WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .set_backlight(context::current(), self.route.clone(), self.change)
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        let _ = updates.send(GuiUpdate::BacklightWritten {
            key: self.key,
            route: self.route,
            write: self.write,
            result: or_unavailable(outcome),
        });
    }
}

/// Name the computer on one Easy-Switch slot of a device now, answered as
/// [`GuiUpdate::EasySwitchHostRenamed`] with the slots the device reads back.
pub struct RenameEasySwitchHost {
    pub route: DeviceRoute,
    /// Zero-based slot index.
    pub slot: u8,
    pub name: String,
    pub key: DeviceKey,
    /// The number the device-read service gave this rename, returned with
    /// the answer so it is matched to the rename it answers.
    pub write: u64,
}

impl Request for RenameEasySwitchHost {
    type Answer = Result<EasySwitchHosts, WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .rename_easy_switch_host(
                context::current(),
                self.route.clone(),
                self.slot,
                self.name.clone(),
            )
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        let _ = updates.send(GuiUpdate::EasySwitchHostRenamed {
            key: self.key,
            route: self.route,
            write: self.write,
            result: or_unavailable(outcome),
        });
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
    key: DeviceKey,
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
    pub key: DeviceKey,
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
    pub key: DeviceKey,
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

/// Read a device's Disable Keys state; the answer goes back over `reply`.
pub struct ReadDisableKeys {
    pub route: DeviceRoute,
    pub reply: oneshot::Sender<Result<DisableKeysState, WriteError>>,
}

impl Request for ReadDisableKeys {
    type Answer = Result<DisableKeysState, WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .read_disable_keys(context::current(), self.route.clone())
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, _: &UpdateSender) {
        let _ = self.reply.send(or_unavailable(outcome));
    }
}

/// Guardedly replace a device's Disable Keys mask and report the confirmed
/// device state to the transaction that requested it.
pub struct SetDisableKeys {
    pub context: DisableKeysRequestContext,
    pub desired: DisableKeysMask,
}

impl Request for SetDisableKeys {
    type Answer = Result<DisableKeysState, WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .set_disable_keys(context::current(), self.context.route.clone(), self.desired)
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        let _ = updates.send(GuiUpdate::DisableKeysWriteResult {
            context: self.context,
            result: or_unavailable(outcome),
        });
    }
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

/// Read a mouse's report rate; the answer goes back over `reply`.
pub struct ReadReportRate {
    pub route: DeviceRoute,
    pub reply: oneshot::Sender<Result<ReportRateInfo, WriteError>>,
}

impl Request for ReadReportRate {
    type Answer = Result<ReportRateInfo, WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .read_report_rate(context::current(), self.route.clone())
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, _: &UpdateSender) {
        let _ = self.reply.send(or_unavailable(outcome));
    }
}

/// Read a mouse's pointer speed; the answer goes back over `reply`.
pub struct ReadPointerSpeed {
    pub route: DeviceRoute,
    pub reply: oneshot::Sender<Result<PointerSpeed, WriteError>>,
}

impl Request for ReadPointerSpeed {
    type Answer = Result<PointerSpeed, WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .read_pointer_speed(context::current(), self.route.clone())
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, _: &UpdateSender) {
        let _ = self.reply.send(or_unavailable(outcome));
    }
}

/// Read the press force of a force-sensing button, with the range the device
/// accepts; the answer goes back over `reply`.
pub struct ReadForceSensing {
    pub route: DeviceRoute,
    pub reply: oneshot::Sender<Result<ForceSensingInfo, WriteError>>,
}

impl Request for ReadForceSensing {
    type Answer = Result<ForceSensingInfo, WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .read_force_sensing(context::current(), self.route.clone())
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, _: &UpdateSender) {
        let _ = self.reply.send(or_unavailable(outcome));
    }
}

/// Read a keyboard's backlight; the answer goes back over `reply`.
pub struct ReadBacklight {
    pub route: DeviceRoute,
    pub reply: oneshot::Sender<Result<BacklightState, WriteError>>,
}

impl Request for ReadBacklight {
    type Answer = Result<BacklightState, WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .read_backlight(context::current(), self.route.clone())
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, _: &UpdateSender) {
        let _ = self.reply.send(or_unavailable(outcome));
    }
}

/// Read a device's Easy-Switch host slots; the answer goes back over
/// `reply`.
pub struct ReadEasySwitchHosts {
    pub route: DeviceRoute,
    pub reply: oneshot::Sender<Result<EasySwitchHosts, WriteError>>,
}

impl Request for ReadEasySwitchHosts {
    type Answer = Result<EasySwitchHosts, WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .read_easy_switch_hosts(context::current(), self.route.clone())
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, _: &UpdateSender) {
        let _ = self.reply.send(or_unavailable(outcome));
    }
}

/// Read a keyboard's Fn-lock state; the answer goes back over `reply`.
pub struct ReadFnLock {
    pub route: DeviceRoute,
    pub reply: oneshot::Sender<Result<FnLockState, WriteError>>,
}

impl Request for ReadFnLock {
    type Answer = Result<FnLockState, WriteError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .read_fn_lock(context::current(), self.route.clone())
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, _: &UpdateSender) {
        let _ = self.reply.send(or_unavailable(outcome));
    }
}

/// Have the agent re-read the `config.toml` the GUI just saved
/// ([`openlogi_ipc::Agent::reload_saved_config`]).
///
/// The loop holds this one until a connection exists and never answers it
/// locally (module doc), so `deliver` only ever reports the agent's own
/// verdict: a reload that did not happen is not one the agent refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReloadConfig {
    /// The controls of lights the user set in the save without a write to
    /// the light. A change made on the light to one of them gives way.
    pub chosen: Vec<ChosenLight>,
    /// The revision of `config.toml` that the save wrote. The agent takes a
    /// file at another revision as edited after the save.
    pub revision: ConfigRevision,
    pub(crate) contexts: Vec<SavedReloadContext>,
}

/// An acknowledgement recipient and the saved revision it needs confirmed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SavedReloadContext {
    /// Application-wide saves retain the aggregate reload result.
    General,
    /// A hardware transaction must confirm its own save, not a later batch member.
    DisableKeys {
        context: DisableKeysRequestContext,
        revision: ConfigRevision,
    },
    /// External adoption must confirm its own revision, not a later batch member.
    Reconciled {
        request_id: u64,
        revision: ConfigRevision,
    },
}

impl SavedReloadContext {
    /// Identity used to replace a retry of the same transaction while batching.
    pub(crate) fn context(&self) -> ConfigReloadContext {
        match self {
            Self::General => ConfigReloadContext::General,
            Self::DisableKeys { context, .. } => ConfigReloadContext::DisableKeys(context.clone()),
            Self::Reconciled { request_id, .. } => ConfigReloadContext::Reconciled(*request_id),
        }
    }

    fn verdict(
        &self,
        submitted: ConfigRevision,
        outcome: Result<SavedConfigVerdict, ConfigReloadError>,
    ) -> Result<SavedConfigVerdict, ConfigReloadError> {
        match (self, outcome) {
            (
                Self::DisableKeys { revision, .. } | Self::Reconciled { revision, .. },
                Ok(SavedConfigVerdict::Accepted),
            ) if *revision != submitted => Ok(SavedConfigVerdict::FileChangedAfterSave),
            (_, outcome) => outcome,
        }
    }
}

impl Default for ReloadConfig {
    fn default() -> Self {
        Self {
            chosen: Vec::new(),
            revision: ConfigRevision::default(),
            contexts: vec![SavedReloadContext::General],
        }
    }
}

impl ReloadConfig {
    /// Confirm an adopted file without changing general-save error presentation.
    pub(crate) fn reconciled(request_id: u64, revision: ConfigRevision) -> Self {
        Self {
            contexts: vec![SavedReloadContext::Reconciled {
                request_id,
                revision,
            }],
            revision,
            ..Default::default()
        }
    }

    /// Also confirm a later general save while external-file reconciliation is pending.
    pub(crate) fn track_reconciliation(&mut self, request_id: u64) {
        self.contexts.push(SavedReloadContext::Reconciled {
            request_id,
            revision: self.revision,
        });
    }

    pub(crate) fn disable_keys(
        context: DisableKeysRequestContext,
        revision: ConfigRevision,
    ) -> Self {
        Self {
            contexts: vec![SavedReloadContext::DisableKeys { context, revision }],
            revision,
            ..Default::default()
        }
    }
}

impl Request for ReloadConfig {
    type Answer = Result<SavedConfigVerdict, ConfigReloadError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        let saved = SavedConfig {
            revision: self.revision,
            chosen: self.chosen.clone(),
        };
        client
            .reload_saved_config_with_verdict(context::current(), saved)
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        report_saved_reload(self.contexts, self.revision, outcome, updates);
    }
}

/// Have the agent re-read a `config.toml` it did not see the GUI save, as at
/// startup, so edits made while the GUI was closed apply
/// ([`openlogi_ipc::Agent::reload_config`]). Held and answered like
/// [`ReloadConfig`].
pub struct LoadConfig;

impl Request for LoadConfig {
    type Answer = Result<(), ConfigReloadError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client.reload_config(context::current()).await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        if let Ok(result) = outcome {
            let _ = updates.send(GuiUpdate::ConfigLoadResult { result });
        }
    }
}

/// Report the agent's verdict on a held reload. A reload that did not happen
/// is not one the agent refused, so it reports nothing.
fn report_saved_reload(
    contexts: impl IntoIterator<Item = SavedReloadContext>,
    submitted: ConfigRevision,
    outcome: Result<Result<SavedConfigVerdict, ConfigReloadError>, Unavailable>,
    updates: &UpdateSender,
) {
    if let Ok(verdict) = outcome {
        for context in contexts {
            let _ = updates.send(GuiUpdate::ConfigReloadResult {
                context: context.context(),
                result: context.verdict(submitted, verdict.clone()),
            });
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

/// End the pairing session, acknowledging only after the agent releases it.
pub struct CancelPairing;

impl Request for CancelPairing {
    type Answer = Result<(), PairingCommandError>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client.cancel_pairing(context::current()).await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        match outcome {
            Ok(Ok(())) => {
                let _ = updates.send(GuiUpdate::PairingCancelled);
            }
            other => report_pairing_refusal(updates, other),
        }
    }
}

/// Remove one pairing from its receiver, answered as
/// [`GuiUpdate::DeviceUnpaired`] so local state changes only once the
/// receiver has let the pairing go.
pub struct UnpairDevice {
    pub route: DeviceRoute,
    /// What the pairing's removal is for.
    pub intent: UnpairIntent,
}

impl Request for UnpairDevice {
    type Answer = Result<(), PairingFailure>;

    async fn call(&self, client: &AgentClient) -> Result<Self::Answer, RpcError> {
        client
            .unpair_device(context::current(), self.route.clone())
            .await
    }

    fn deliver(self, outcome: Result<Self::Answer, Unavailable>, updates: &UpdateSender) {
        let result = match outcome {
            Ok(Ok(())) => Ok(()),
            Ok(Err(failure)) => Err(UnpairFailure::Refused(failure)),
            Err(Unavailable) => Err(UnpairFailure::AgentUnreachable),
        };
        let _ = updates.send(GuiUpdate::DeviceUnpaired {
            route: self.route,
            intent: self.intent,
            result,
        });
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
    SetFnLock,
    SetReportRate,
    SetPointerSpeed,
    SetForceSensing,
    SetBacklight,
    RenameEasySwitchHost,
    ReadDpi,
    ReadSmartShift,
    ReadFnLock,
    ReadReportRate,
    ReadPointerSpeed,
    ReadForceSensing,
    ReadBacklight,
    ReadEasySwitchHosts,
    LoadConfig,
    ReadDisableKeys,
    SetDisableKeys,
    ReloadConfig,
    RequestAccessibilityPrompt,
    StartPairing,
    PairDevice,
    CancelPairing,
    UnpairDevice,
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
    fn a_cancel_that_found_no_agent_releases_the_local_wait_with_an_error() {
        let (updates, mut received) = mpsc::unbounded_channel();

        CancelPairing.deliver(Err(Unavailable), &updates);

        assert!(matches!(
            received.try_recv(),
            Ok(GuiUpdate::PairingUndeliverable(
                PairingFailure::AgentRestarted
            ))
        ));
    }

    #[test]
    fn a_completed_cancel_acknowledges_even_without_a_snapshot_change() {
        let (updates, mut received) = mpsc::unbounded_channel();
        CancelPairing.deliver(Ok(Ok(())), &updates);
        assert!(matches!(
            received.try_recv(),
            Ok(GuiUpdate::PairingCancelled)
        ));
    }

    #[test]
    fn a_reload_that_never_reached_the_agent_is_not_a_verdict() {
        let (updates, mut received) = mpsc::unbounded_channel();

        ReloadConfig::default().deliver(Err(Unavailable), &updates);
        LoadConfig.deliver(Err(Unavailable), &updates);

        assert!(received.try_recv().is_err());
    }
}
