//! USB microphones as the agent reports them, and the control writes the
//! settings app sends them.
//!
//! The agent re-reads every microphone about once a second and after each
//! write, so the list here is the device's own state; nothing is kept in
//! `config.toml`. The OS remembers a USB audio device's levels itself, and a
//! second copy would only fight it.

use openlogi_core::microphone::{Microphone, MicrophoneChange, MicrophoneError};

use super::{AppState, DeviceRecord, StateEvent, StateEvents};

/// The key a microphone's device record is filed under. Device records and
/// the panel both derive it here, so they cannot disagree.
pub(crate) fn microphone_record_key(id: &str) -> String {
    format!("microphone:{id}")
}

/// The microphones from the latest snapshot, and the last refused write.
#[derive(Debug, Default)]
pub(crate) struct MicrophoneSession {
    list: Vec<Microphone>,
    failure: Option<Failure>,
}

/// A write a microphone refused, kept until the next write to it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Failure {
    id: String,
    error: MicrophoneError,
}

impl AppState {
    /// The connected microphones, as the agent last read them.
    pub(crate) fn microphones(&self) -> &[Microphone] {
        &self.microphones.list
    }

    /// The microphone behind `record`, while it is connected.
    pub(crate) fn microphone_for(&self, record: &DeviceRecord) -> Option<&Microphone> {
        self.microphones
            .list
            .iter()
            .find(|microphone| microphone_record_key(&microphone.id) == record.config_key)
    }

    /// Why the last write to microphone `id` was refused, if it was.
    pub(crate) fn microphone_failure(&self, id: &str) -> Option<MicrophoneError> {
        self.microphones
            .failure
            .as_ref()
            .filter(|failure| failure.id == id)
            .map(|failure| failure.error)
    }

    /// Adopt the microphones a snapshot carries.
    pub(super) fn set_microphones(&mut self, microphones: &[Microphone]) -> StateEvents {
        if self.microphones.list == microphones {
            return StateEvents::none();
        }
        self.microphones.list = microphones.to_vec();
        StateEvent::MicrophoneChanged.into()
    }

    /// Ask the agent to apply `change` to microphone `id`.
    pub(crate) fn change_microphone(&mut self, id: &str, change: MicrophoneChange) -> StateEvents {
        let cleared = self
            .microphones
            .failure
            .take_if(|failure| failure.id == id)
            .is_some();
        self.send_ipc(crate::services::ipc::SetMicrophone {
            id: id.to_owned(),
            change,
        });
        if cleared {
            StateEvent::MicrophoneChanged.into()
        } else {
            StateEvents::none()
        }
    }

    /// Fold in the agent's answer to a [`Self::change_microphone`]: what the
    /// microphone read back, or why it refused. `None` means the agent was
    /// unreachable; the next snapshot shows the microphone as it is.
    pub(crate) fn apply_microphone_written(
        &mut self,
        id: String,
        result: Option<Result<Microphone, MicrophoneError>>,
    ) -> StateEvents {
        match result {
            Some(Ok(microphone)) => {
                let Some(listed) = self
                    .microphones
                    .list
                    .iter_mut()
                    .find(|listed| listed.id == microphone.id)
                else {
                    return StateEvents::none();
                };
                if *listed == microphone {
                    return StateEvents::none();
                }
                *listed = microphone;
            }
            Some(Err(error)) => self.microphones.failure = Some(Failure { id, error }),
            None => return StateEvents::none(),
        }
        StateEvent::MicrophoneChanged.into()
    }
}
