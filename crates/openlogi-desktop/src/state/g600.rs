//! Logitech G600 onboard DPI: four DPI stages per onboard profile and the
//! stage the mouse is on.
//!
//! The mouse stores its stages in onboard memory, so nothing is persisted
//! here and nothing is replayed when it reconnects. The reading is the
//! swr-backed device query the device-read service owns. A write goes to the
//! agent, which keeps every other request away from the mouse while it
//! stores a profile; one write per mouse is in flight at a time, and the
//! mouse is read again once it answers, whether it landed or not. A write
//! that landed keeps the mouse busy until that reading is in, so the card
//! never offers the reading from before the write as current.
//!
//! The mouse's own buttons switch its stage and profile without telling the
//! agent, so the card reads it again every [`G600_RECONCILE_INTERVAL`] while
//! it shows it, through [`AppState::reconcile_g600_dpi`].

use std::time::Duration;

use openlogi_core::hid::g600::{
    DpiSettings as G600DpiSettings, DpiStage as G600DpiStage, G600_DRIVER_ID,
    OnboardProfile as G600Profile, StageAfterStore as G600StageAfterStore,
};
use openlogi_core::hid::{DeviceRoute, WriteError};
use tracing::{debug, warn};

use super::device_key::DeviceKey;
use super::devices::DeviceRecord;
use super::events::StateEvents;
use super::load::G600DpiLoad;
use super::{AppState, StateEvent};

/// How long the card shows a G600's reading before it reads the mouse again
/// to catch a stage or profile its own buttons switched.
pub(crate) const G600_RECONCILE_INTERVAL: Duration = Duration::from_secs(2);

/// The G600 write the GUI sent last, while it is in flight, while the mouse
/// is read again after it landed, or after it failed. A write that landed
/// leaves no status once that reading is in.
///
/// [`Self::Confirming`] is never stored: the device-read service owns the
/// reading after a write, and [`AppState::current_g600_write`] reports it
/// for as long as that reading is on its way, so it ends with that query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum G600WriteStatus {
    /// DPI stages are being stored in this profile, and the mouse put on
    /// the stage the write names; the mouse needs over a second to store
    /// them.
    SavingProfile(G600Profile),
    /// The mouse is being switched to this stage of its active profile.
    SelectingStage(G600DpiStage),
    /// The write landed, and the mouse is being read again to show what it
    /// now holds.
    Confirming,
    /// Storing DPI stages in this profile, or selecting the stage after
    /// them, failed or could not be confirmed. The error says how far it
    /// got: a profile may be stored even though the stage after it failed.
    ProfileFailed(G600Profile, WriteError),
    /// Switching the active stage failed or could not be confirmed. The
    /// error says how far it got: a selection may have landed on another
    /// profile than the one it was meant for.
    StageFailed(WriteError),
}

impl G600WriteStatus {
    /// Whether the mouse is still busy with a write: storing it, or being
    /// read again after it.
    #[must_use]
    pub fn in_flight(&self) -> bool {
        matches!(
            self,
            Self::SavingProfile(_) | Self::SelectingStage(_) | Self::Confirming
        )
    }
}

impl AppState {
    /// Whether the active device is a G600, whose DPI lives in its onboard
    /// profiles rather than behind HID++.
    #[must_use]
    pub fn current_g600_dpi_supported(&self) -> bool {
        self.current_record()
            .is_some_and(DeviceRecord::has_g600_onboard_dpi)
    }

    /// The route to the active G600, or `None` when the active device is no
    /// G600 or it is offline.
    #[must_use]
    pub fn current_g600_route(&self) -> Option<&DeviceRoute> {
        self.writable_g600().map(|(_, route)| route)
    }

    /// What is known of the active G600's onboard DPI.
    #[must_use]
    pub fn current_g600_dpi_load(&self) -> G600DpiLoad {
        self.current_record()
            .and_then(|record| self.pointer.reads.g600_dpi_load(&record.device_key()))
            .cloned()
            .unwrap_or_default()
    }

    /// The write the active G600 is waiting on, or why its last one failed.
    #[must_use]
    pub fn current_g600_write(&self) -> Option<G600WriteStatus> {
        let key = self.current_record().map(DeviceRecord::device_key)?;
        self.g600_write(&key)
    }

    /// Store `settings` in onboard `profile` of the active G600 through the
    /// agent, which puts the mouse on the stage `after` asks for in the same
    /// operation: a save may renumber the stages, so the stage slot the
    /// mouse was using can come to hold another DPI. The answer arrives as
    /// [`Self::apply_g600_dpi_written`]. No-op when no G600 is selected, it
    /// is offline, or a write to it is still in flight.
    pub fn commit_g600_profile_dpi(
        &mut self,
        profile: G600Profile,
        settings: G600DpiSettings,
        after: G600StageAfterStore,
    ) -> StateEvents {
        let Some((key, route)) = self.idle_g600() else {
            return StateEvents::none();
        };
        debug!(key = %key, profile = profile.number(), ?after, "storing G600 DPI stages");
        self.pointer
            .g600_writes
            .insert(key.clone(), G600WriteStatus::SavingProfile(profile));
        self.send_ipc(crate::services::ipc::SetG600ProfileDpi {
            route,
            profile,
            settings,
            after,
            key: key.clone(),
        });
        StateEvent::G600DpiChanged(key).into()
    }

    /// Switch the active G600 to `stage` of its active profile through the
    /// agent. Same answer and no-op cases as
    /// [`Self::commit_g600_profile_dpi`].
    pub fn commit_g600_active_stage(&mut self, stage: G600DpiStage) -> StateEvents {
        let Some((key, route)) = self.idle_g600() else {
            return StateEvents::none();
        };
        debug!(key = %key, stage = stage.number(), "selecting G600 DPI stage");
        self.pointer
            .g600_writes
            .insert(key.clone(), G600WriteStatus::SelectingStage(stage));
        self.send_ipc(crate::services::ipc::SetG600ActiveStage {
            route,
            stage,
            key: key.clone(),
        });
        StateEvent::G600DpiChanged(key).into()
    }

    /// The agent's answer to a G600 write: read the mouse again, since only
    /// it knows what it stored, and stay busy until that reading is in when
    /// the write landed, or keep the error against what was written when it
    /// failed. A failure is logged as the agent reports it, since part of
    /// the write may have landed.
    pub fn apply_g600_dpi_written(
        &mut self,
        key: &DeviceKey,
        result: Result<(), WriteError>,
    ) -> StateEvents {
        let written = self.pointer.g600_writes.remove(key);
        let status = match (result, written) {
            (Ok(()), _) => {
                self.pointer.reads.confirm_g600_dpi(key);
                None
            }
            (Err(error), Some(G600WriteStatus::SavingProfile(profile))) => {
                warn!(%error, key = %key, "G600 profile write failed");
                Some(G600WriteStatus::ProfileFailed(profile, error))
            }
            (Err(error), Some(G600WriteStatus::SelectingStage(_))) => {
                warn!(%error, key = %key, "G600 stage selection failed");
                Some(G600WriteStatus::StageFailed(error))
            }
            (Err(error), written) => {
                warn!(%error, key = %key, ?written, "G600 write answer with no write in flight");
                None
            }
        };
        if let Some(status) = status {
            self.pointer.reads.reread_g600_dpi(key);
            self.pointer.g600_writes.insert(key.clone(), status);
        }
        StateEvent::G600DpiChanged(key.clone()).into()
    }

    /// Re-run `key`'s exhausted onboard-DPI read — the "click to retry"
    /// affordance on a failed G600 read.
    pub(crate) fn retry_g600_dpi_read(&mut self, key: &DeviceKey) -> StateEvents {
        self.pointer.reads.retry_g600_dpi(key);
        StateEvent::G600DpiChanged(key.clone()).into()
    }

    /// Read `key`'s G600 again, keeping its reading on screen while it
    /// answers: its DPI and profile buttons change what it uses without a
    /// word to the agent. Skipped while a write is in flight, since the
    /// reading after it is already on its way; the agent keeps the read
    /// away from the mouse while it stores a profile.
    pub(crate) fn reconcile_g600_dpi(&mut self, key: &DeviceKey) {
        if !self.g600_writing(key) {
            self.pointer.reads.refresh_g600_dpi(key);
        }
    }

    /// `key`'s G600 came back online: read it again, since another computer
    /// may have changed its stages meanwhile.
    pub(super) fn g600_dpi_reconnected(&mut self, key: &DeviceKey) {
        self.pointer.reads.refresh_g600_dpi(key);
    }

    pub(super) fn load_current_g600_dpi(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| record.has_g600_onboard_dpi())
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        debug!(key = %key, "reading the G600's onboard DPI");
        self.pointer
            .reads
            .ensure_g600_dpi(key, route, self.ipc_sender(), cx);
    }

    fn writable_g600(&self) -> Option<(&DeviceRecord, &DeviceRoute)> {
        let record = self
            .current_record()
            .filter(|record| record.has_g600_onboard_dpi())?;
        Some((record, record.addressable_route()?))
    }

    /// The write `key`'s G600 is waiting on, or why its last one failed.
    fn g600_write(&self, key: &DeviceKey) -> Option<G600WriteStatus> {
        self.pointer.g600_writes.get(key).cloned().or_else(|| {
            self.pointer
                .reads
                .g600_confirming(key)
                .then_some(G600WriteStatus::Confirming)
        })
    }

    /// Whether `key`'s G600 is still busy with a write.
    fn g600_writing(&self, key: &DeviceKey) -> bool {
        self.g600_write(key)
            .as_ref()
            .is_some_and(G600WriteStatus::in_flight)
    }

    /// The active G600's key and route, when it can take a write now.
    fn idle_g600(&self) -> Option<(DeviceKey, DeviceRoute)> {
        let (record, route) = self.writable_g600()?;
        let key = record.device_key();
        if self.g600_writing(&key) {
            debug!(key = %key, "G600 write already in flight — ignoring another");
            return None;
        }
        Some((key, route.clone()))
    }
}

impl DeviceRecord {
    /// Whether this is a G600, whose DPI stages live in its onboard
    /// profiles: its standalone driver measured that, not its kind.
    #[must_use]
    pub(crate) fn has_g600_onboard_dpi(&self) -> bool {
        self.driver_id.as_deref() == Some(G600_DRIVER_ID)
    }
}
