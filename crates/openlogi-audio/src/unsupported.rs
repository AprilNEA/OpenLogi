//! Platforms without a microphone backend yet: nothing is found, and every
//! write is refused as unsupported.

use openlogi_core::microphone::{Microphone, MicrophoneChange, MicrophoneError};

#[expect(
    clippy::unnecessary_wraps,
    reason = "matches the CoreAudio backend's signature, which can fail"
)]
pub(crate) fn microphones() -> Result<Vec<Microphone>, MicrophoneError> {
    Ok(Vec::new())
}

pub(crate) fn apply(_id: &str, _change: MicrophoneChange) -> Result<Microphone, MicrophoneError> {
    Err(MicrophoneError::PlatformUnsupported)
}
