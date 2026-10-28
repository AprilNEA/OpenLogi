//! The macOS Secure Event Input watcher.
//!
//! While any process holds Secure Event Input — a focused password field,
//! Terminal's Secure Keyboard Entry, some password managers — macOS delivers
//! no key presses to the event tap, so key bindings made through the OS hook
//! stop firing. macOS posts no notification when it turns on or off, so this
//! samples it on a timer, at the rate Logi Options+ uses, and reports changes.

use std::thread;
use std::time::Duration;

use openlogi_core::app::ForegroundApp;
use openlogi_ipc::SecureInput;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// How often the watcher samples Secure Event Input. Logi Options+ 2.8 polls
/// it every 2 s as well.
pub const PERIOD: Duration = Duration::from_secs(2);

/// Start the watcher: one sample now and one every `period`. It sends the
/// settled state at the first sample and at each change after it, and stops
/// within one period once the receiver is dropped.
///
/// Off macOS there is no Secure Event Input: the receiver closes without a
/// value, and the status keeps its default, [`SecureInput::Off`].
#[must_use]
pub fn spawn(period: Duration) -> mpsc::UnboundedReceiver<SecureInput> {
    let (tx, rx) = mpsc::unbounded_channel();
    if !cfg!(target_os = "macos") {
        return rx;
    }
    let spawn_result = thread::Builder::new()
        .name("openlogi-secure-input-watcher".into())
        .spawn(move || {
            let mut settle = Settle::default();
            while !tx.is_closed() {
                let sample = sample(
                    openlogi_hook::secure_input_enabled,
                    openlogi_hook::secure_input_app,
                );
                if let Some(state) = settle.observe(sample) {
                    log_change(&state);
                    if tx.send(state).is_err() {
                        break;
                    }
                }
                thread::sleep(period);
            }
            debug!("secure-input watcher receiver dropped — exiting");
        });
    if let Err(error) = spawn_result {
        warn!(error = %error, "could not spawn the secure-input watcher");
    }
    rx
}

/// Read the current state. The app is read only while it is on: the window
/// server publishes the pid only then.
fn sample(
    enabled: impl FnOnce() -> bool,
    app: impl FnOnce() -> Option<ForegroundApp>,
) -> SecureInput {
    if enabled() {
        SecureInput::On(app())
    } else {
        SecureInput::Off
    }
}

fn log_change(state: &SecureInput) {
    match state {
        SecureInput::Off => info!("Secure Event Input is off — key bindings fire again"),
        SecureInput::On(app) => info!(
            app = app.as_ref().map_or("unknown", |app| app.id.as_str()),
            "Secure Event Input is on — macOS hides key presses from the hook"
        ),
    }
}

/// Turns samples into the state to report.
///
/// "On" waits until two consecutive samples see it, so focusing a password
/// field for a moment does not flash a notice; Options+ likewise waits 2 s
/// before it shows its toast. "Off", and a change of app while on, report at
/// once.
#[derive(Debug, Default)]
struct Settle {
    /// Whether the previous sample saw Secure Event Input on.
    was_on: bool,
    /// The state last reported; `None` before the first report.
    reported: Option<SecureInput>,
}

impl Settle {
    /// Fold in one sample. `Some` when the settled state changed, and always
    /// at the first sample.
    fn observe(&mut self, sample: SecureInput) -> Option<SecureInput> {
        let on = matches!(sample, SecureInput::On(_));
        let settled = if on && !self.was_on {
            SecureInput::Off
        } else {
            sample
        };
        self.was_on = on;
        if self.reported.as_ref() == Some(&settled) {
            return None;
        }
        self.reported = Some(settled.clone());
        Some(settled)
    }
}

#[cfg(test)]
mod tests {
    use super::{Settle, sample};
    use openlogi_core::app::ForegroundApp;
    use openlogi_ipc::SecureInput;

    fn on(id: &str) -> SecureInput {
        SecureInput::On(Some(ForegroundApp::unnamed(id.to_string())))
    }

    #[test]
    fn the_first_sample_is_always_reported() {
        assert_eq!(
            Settle::default().observe(SecureInput::Off),
            Some(SecureInput::Off)
        );
    }

    #[test]
    fn on_is_reported_only_from_the_second_consecutive_sample() {
        let mut settle = Settle::default();
        assert_eq!(settle.observe(SecureInput::Off), Some(SecureInput::Off));
        assert_eq!(settle.observe(on("com.apple.Terminal")), None);
        assert_eq!(
            settle.observe(on("com.apple.Terminal")),
            Some(on("com.apple.Terminal"))
        );
        assert_eq!(settle.observe(on("com.apple.Terminal")), None);
    }

    #[test]
    fn on_at_the_first_sample_reports_off_first() {
        let mut settle = Settle::default();
        assert_eq!(
            settle.observe(on("com.apple.Terminal")),
            Some(SecureInput::Off)
        );
        assert_eq!(
            settle.observe(on("com.apple.Terminal")),
            Some(on("com.apple.Terminal"))
        );
    }

    #[test]
    fn a_single_on_sample_between_offs_is_never_reported() {
        let mut settle = Settle::default();
        assert_eq!(settle.observe(SecureInput::Off), Some(SecureInput::Off));
        assert_eq!(settle.observe(on("com.apple.Safari")), None);
        assert_eq!(settle.observe(SecureInput::Off), None);
        assert_eq!(settle.observe(on("com.apple.Safari")), None);
        assert_eq!(settle.observe(SecureInput::Off), None);
    }

    #[test]
    fn off_and_a_new_app_while_on_report_at_once() {
        let mut settle = Settle::default();
        settle.observe(on("com.apple.Terminal"));
        settle.observe(on("com.apple.Terminal"));
        assert_eq!(
            settle.observe(on("com.apple.Safari")),
            Some(on("com.apple.Safari"))
        );
        assert_eq!(settle.observe(SecureInput::Off), Some(SecureInput::Off));
    }

    #[test]
    fn the_app_is_read_only_while_secure_input_is_on() {
        let app = || -> Option<ForegroundApp> { panic!("read the app while off") };
        assert_eq!(sample(|| false, app), SecureInput::Off);
        assert_eq!(
            sample(|| true, || Some(ForegroundApp::unnamed("x".to_string()))),
            on("x")
        );
        assert_eq!(sample(|| true, || None), SecureInput::On(None));
    }
}
