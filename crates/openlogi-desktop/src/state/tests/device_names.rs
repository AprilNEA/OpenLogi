//! Custom device names, including two serial-less cameras of one model.

use super::*;
use crate::services::assets::ResolvedAsset;

const CAMERA_A_ID: &str = "0x1123000046d0893";

const CAMERA_B_ID: &str = "0x14110000046d0893";

const SYNTHETIC_MOUSE_KEY: &str = "unit:4f4c4403";

fn synthetic_mouse_inventory() -> DeviceInventory {
    let mut inventory = direct_inventory([0x4f, 0x4c, 0x44, 0x03]);
    inventory.paired[0].codename = Some("Synthetic Direct Mouse".to_string());
    // Keep physical model_info so the live device resolves to its persistent
    // key (`unit:4f4c4403`), while an unknown model id makes asset lookup miss.
    if let Some(model) = inventory.paired[0].model_info.as_mut() {
        model.model_ids = [0xfffe, 0, 0];
        model.extended_model_id = 0;
    }
    inventory
}

fn product_identity(inventory: &DeviceInventory) -> DeviceIdentity {
    DeviceIdentity {
        display_name: "MX Vertical".to_string(),
        model_info: inventory.paired[0].model_info.clone(),
        codename: inventory.paired[0].codename.clone(),
        kind: DeviceKind::Mouse,
        // Intentionally differs from `inventory.paired[0].capabilities` so
        // `persist_identities` must execute an actual write and return `true`.
        capabilities: Capabilities::default(),
        light_capabilities: None,
        driver_id: None,
        registry_model_id: None,
    }
}

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

#[test]
fn gallery_keeps_persisted_product_name_while_asset_lookup_misses() {
    let inventory = synthetic_mouse_inventory();
    let mut config = Config::ephemeral();
    config.set_device_identity(SYNTHETIC_MOUSE_KEY, product_identity(&inventory));
    let resolver = AssetResolver::empty();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: std::slice::from_ref(&inventory),
        ..Sources::in_memory(config, &resolver, commands)
    });

    assert_eq!(state.devices().len(), 1);
    let record = &state.devices()[0];
    assert!(record.is_online());
    assert!(record.is_persistent());
    assert_eq!(record.config_key, SYNTHETIC_MOUSE_KEY);
    assert_eq!(record.canonical_key.as_deref(), Some(SYNTHETIC_MOUSE_KEY));
    assert_eq!(record.sighting_key(), Some(SYNTHETIC_MOUSE_KEY));
    assert_eq!(record.asset, None);
    assert_eq!(record.display_name, "MX Vertical");
    assert_eq!(record.model_name, "MX Vertical");
}

#[test]
fn persist_identities_keeps_persisted_product_name_without_resolved_asset() {
    let inventory = synthetic_mouse_inventory();
    let mut config = Config::ephemeral();
    config.set_device_identity(SYNTHETIC_MOUSE_KEY, product_identity(&inventory));
    let resolver = AssetResolver::empty();

    let mut records = build_device_list(
        std::slice::from_ref(&inventory),
        &[],
        &resolver,
        &config,
        &[],
    );
    assert_eq!(records.len(), 1);
    assert!(records[0].is_online());
    assert!(records[0].is_persistent());
    assert_eq!(records[0].sighting_key(), Some(SYNTHETIC_MOUSE_KEY));
    assert_eq!(records[0].asset, None);

    // Simulate a record whose model name fell back to the codename before the
    // asset resolved: persisting must keep the already-saved product name.
    records[0].model_name = "Synthetic Direct Mouse".to_string();
    let changed = super::super::inventory::persist_identities(&mut config, &records);
    assert!(changed);
    let persisted = config
        .device_identity(SYNTHETIC_MOUSE_KEY)
        .expect("persisted synthetic mouse identity");
    assert_eq!(persisted.display_name, "MX Vertical");
    assert_eq!(
        persisted.capabilities,
        Capabilities::presumed_from_kind(DeviceKind::Mouse)
    );

    // Once the asset resolves, persisting updates the stored product name.
    records[0].asset = Some(ResolvedAsset {
        depot: "mx_vertical".to_string(),
        display_name: "MX Vertical Ergonomic".to_string(),
        kind: Some(DeviceKind::Mouse),
        image_path: std::path::PathBuf::from("/tmp/mx-vertical.png"),
        hero_image_path: None,
        glow: None,
        metadata: openlogi_assets::Metadata::default(),
        png_width: 640,
        png_height: 480,
        side_view: None,
    });
    records[0].model_name = "MX Vertical Ergonomic".to_string();
    let changed = super::super::inventory::persist_identities(&mut config, &records);
    assert!(changed);
    assert_eq!(
        config
            .device_identity(SYNTHETIC_MOUSE_KEY)
            .map(|identity| identity.display_name.as_str()),
        Some("MX Vertical Ergonomic")
    );
}
