use openlogi_core::hid::HapticPulse;

use super::*;

fn command(waveform: Waveform) -> ApiCommand {
    ApiCommand::Haptic {
        device: route().to_string(),
        waveform,
    }
}

/// The agent receives `pulse` for the test mouse and answers `result`.
fn play(pulse: HapticPulse, result: Result<(), WriteError>) -> Step {
    Box::new(move |request| {
        let AgentRequest::PlayHaptic {
            route: requested,
            pulse: asked,
        } = request
        else {
            panic!("expected a haptic play, got {request:?}");
        };
        assert_eq!((requested, asked), (route(), pulse));
        AgentResponse::PlayHaptic(result)
    })
}

#[tokio::test]
async fn a_play_answers_the_device_and_waveform() {
    for (waveform, pulse, name) in [
        (Waveform::Tick, HapticPulse::SubtleCollision, "tick"),
        (Waveform::Confirm, HapticPulse::DampStateChange, "confirm"),
    ] {
        let result = run_script(snapshot(), command(waveform), vec![play(pulse, Ok(()))]).await;
        assert_eq!(
            result,
            json!({
                "schema_version": 1,
                "ok": true,
                "data": { "device": route().to_string(), "waveform": name },
            })
        );
    }
}

#[tokio::test]
async fn the_agents_refusals_keep_their_stable_codes() {
    for (error, code) in [
        (
            WriteError::HapticWaveformUnsupported { waveform: 1 },
            "unsupported_feature",
        ),
        (
            WriteError::FeatureUnsupported {
                feature_hex: 0x19b0,
            },
            "unsupported_feature",
        ),
        (WriteError::DeviceUnreachable { index: 3 }, "device_offline"),
        (WriteError::HapticsUnavailable { status: 2 }, "device_error"),
    ] {
        let result = run_script(
            snapshot(),
            command(Waveform::Confirm),
            vec![play(HapticPulse::DampStateChange, Err(error))],
        )
        .await;
        assert_eq!(result["ok"], json!(false));
        assert_eq!(result["error"]["code"], json!(code));
    }
}

#[tokio::test]
async fn an_offline_device_and_save_are_refused_before_any_play() {
    let mut offline = snapshot();
    offline.inventory[0].paired[0].online = false;
    let result = run_script(offline, command(Waveform::Tick), vec![]).await;
    assert_eq!(result["error"]["code"], json!("device_offline"));

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(openlogi_core::paths::CONFIG_FILE);
    let result = run_script_at(snapshot(), command(Waveform::Tick), vec![], Some(&path)).await;
    assert_eq!(result["error"]["code"], json!("invalid_value"));
}
