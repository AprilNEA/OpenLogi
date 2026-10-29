//! USB microphone watcher.
//!
//! Re-reads every recognized microphone and its controls through
//! `openlogi-audio` on a short period. A level can change without OpenLogi —
//! the mute button on a Yeti, the system's sound settings, another app — and
//! the audio stack offers no notification this agent could rely on for all of
//! them, so the poll is the authority. A sample equal to the last one sends
//! nothing, so the snapshot only moves when something did.

use std::thread;
use std::time::Duration;

use openlogi_core::microphone::{Microphone, MicrophoneError};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// How often the microphones are re-read.
pub const PERIOD: Duration = Duration::from_secs(1);

/// What to do with one poll of the audio stack.
#[derive(Default)]
struct Sampler {
    last: Option<Vec<Microphone>>,
    failing: bool,
}

impl Sampler {
    /// The list to publish after `sample`, or `None` when nothing changed. A
    /// failed read keeps the last list: one refused query is not a microphone
    /// unplugged.
    fn observe(
        &mut self,
        sample: Result<Vec<Microphone>, MicrophoneError>,
    ) -> Option<Vec<Microphone>> {
        match sample {
            Ok(microphones) => {
                self.failing = false;
                if self.last.as_ref() == Some(&microphones) {
                    return None;
                }
                self.last = Some(microphones.clone());
                Some(microphones)
            }
            Err(error) => {
                if !self.failing {
                    warn!(%error, "microphone read failed — keeping the last reading");
                }
                self.failing = true;
                None
            }
        }
    }
}

/// Start the watcher. The first successful read is sent at once; later ones
/// only when they differ. Dropping the receiver stops the worker on its next
/// send. Without an audio backend the channel closes at once.
#[must_use]
pub fn spawn(period: Duration) -> mpsc::UnboundedReceiver<Vec<Microphone>> {
    let (tx, rx) = mpsc::unbounded_channel();
    if !openlogi_audio::BACKEND_AVAILABLE {
        return rx;
    }
    let spawned = thread::Builder::new()
        .name("openlogi-microphone-watcher".into())
        .spawn(move || {
            let mut sampler = Sampler::default();
            loop {
                if let Some(microphones) = sampler.observe(openlogi_audio::microphones())
                    && tx.send(microphones).is_err()
                {
                    debug!("microphone watcher receiver dropped — exiting");
                    return;
                }
                thread::sleep(period);
            }
        });
    if let Err(error) = spawned {
        warn!(%error, "could not spawn microphone watcher");
    }
    rx
}

#[cfg(test)]
mod tests {
    use openlogi_core::microphone::{Microphone, MicrophoneControls, MicrophoneError};

    use super::Sampler;

    fn yeti(muted: bool) -> Microphone {
        Microphone {
            id: "yeti".into(),
            vendor_id: 0x046d,
            product_id: 0x0ab7,
            name: "Yeti".into(),
            registry_model_id: Some("0ab7".into()),
            controls: MicrophoneControls {
                muted: Some(muted),
                ..MicrophoneControls::default()
            },
        }
    }

    #[test]
    fn the_first_read_is_sent_even_when_empty() {
        let mut sampler = Sampler::default();

        assert_eq!(sampler.observe(Ok(Vec::new())), Some(Vec::new()));
    }

    #[test]
    fn an_unchanged_read_sends_nothing() {
        let mut sampler = Sampler::default();
        sampler.observe(Ok(vec![yeti(false)]));

        assert_eq!(sampler.observe(Ok(vec![yeti(false)])), None);
        assert_eq!(
            sampler.observe(Ok(vec![yeti(true)])),
            Some(vec![yeti(true)])
        );
        assert_eq!(sampler.observe(Ok(Vec::new())), Some(Vec::new()));
    }

    #[test]
    fn a_failed_read_keeps_the_last_list() {
        let mut sampler = Sampler::default();
        sampler.observe(Ok(vec![yeti(false)]));

        assert_eq!(sampler.observe(Err(MicrophoneError::Rejected(-1))), None);
        assert_eq!(sampler.observe(Err(MicrophoneError::Rejected(-1))), None);
        // Recovering with the same reading is not a change either.
        assert_eq!(sampler.observe(Ok(vec![yeti(false)])), None);
    }
}
