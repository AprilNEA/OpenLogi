//! Custom device names, including two serial-less cameras of one model.

use super::*;

const CAMERA_A_ID: &str = "0x1123000046d0893";

const CAMERA_B_ID: &str = "0x14110000046d0893";

fn serial_less_same_model_cameras() -> [Camera; 2] {
    let first = Camera {
        name: "Logitech StreamCam".to_string(),
        unique_id: CAMERA_A_ID.to_string(),
        serial_number: None,
        vendor_id: 0x046d,
        product_id: 0x0893,
        max_resolution: None,
        max_fps: None,
    };
    let second = Camera {
        unique_id: CAMERA_B_ID.to_string(),
        ..first.clone()
    };
    [first, second]
}

fn state_with_same_model_cameras(config: Config) -> AppState {
    let cameras = serial_less_same_model_cameras();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    AppState::new(Sources {
        cameras: &cameras,
        ..Sources::in_memory(config, &AssetResolver::new(), commands)
    })
}

fn camera_record<'a>(state: &'a AppState, capture_id: &str) -> &'a DeviceRecord {
    state
        .devices()
        .iter()
        .find(|record| record.capture_id.as_deref() == Some(capture_id))
        .expect("camera record")
}

#[test]
fn custom_device_name_updates_the_ui_and_can_restore_the_model_name() {
    let mut state = state_with_a_known_mouse();
    let model_name = state
        .current_record()
        .expect("known mouse")
        .model_name
        .clone();

    let _ = state.commit_device_custom_name(KNOWN_MOUSE_KEY, "  Office mouse  ");

    assert_eq!(
        state
            .current_record()
            .map(|record| record.display_name.as_str()),
        Some("Office mouse")
    );
    assert_eq!(
        state.config.device_custom_name(KNOWN_MOUSE_KEY),
        Some("Office mouse")
    );

    let _ = state.commit_device_custom_name(KNOWN_MOUSE_KEY, "   ");

    assert_eq!(
        state
            .current_record()
            .map(|record| record.display_name.as_str()),
        Some(model_name.as_str())
    );
    assert_eq!(state.config.device_custom_name(KNOWN_MOUSE_KEY), None);
}

#[test]
fn same_model_serial_less_cameras_keep_independent_names() {
    let mut state = state_with_same_model_cameras(Config::ephemeral());
    let second_key = camera_record(&state, CAMERA_B_ID).record_key();

    let _ = state.commit_device_custom_name(&second_key, "Desk camera");

    assert_eq!(
        camera_record(&state, CAMERA_A_ID).display_name,
        "Logitech StreamCam"
    );
    assert_eq!(
        camera_record(&state, CAMERA_B_ID).display_name,
        "Desk camera"
    );

    let restored = state_with_same_model_cameras(state.config.clone());
    assert_eq!(
        camera_record(&restored, CAMERA_A_ID).display_name,
        "Logitech StreamCam"
    );
    assert_eq!(
        camera_record(&restored, CAMERA_B_ID).display_name,
        "Desk camera"
    );
}
