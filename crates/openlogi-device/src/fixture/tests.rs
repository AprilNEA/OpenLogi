use std::sync::Arc;

use futures_lite::StreamExt as _;
use hidpp::channel::RawHidChannel;
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, DeviceInventory, DeviceKind,
    PairedDevice, ReceiverInfo,
};

use crate::backend::{HidBackend, RawWriter};
use crate::{
    BacklightMode, BacklightState, BacklightStatus, DeviceRoute, Dpi, DpiCapabilities, DpiInfo,
    Enumerator, HotplugEvent, NodeId, NodeInfo, get_dpi,
};

use super::*;

const DIRECT_CHANNEL: &str = "direct-mouse";
const RECEIVER_CHANNEL: &str = "bolt-receiver";
const RECEIVER_UID: &str = "A1B2C3D4E5F60708";

struct SyntheticFixture {
    profile: DeviceProfile,
    topology: ReplayTopology,
    cassette: HidCassette,
    route: DeviceRoute,
    node_id: NodeId,
    channel: &'static str,
}

#[test]
fn canonical_device_profile_is_valid_synthetic_and_privacy_safe() {
    let profile: DeviceProfile =
        serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile parses");
    profile.validate().expect("canonical profile validates");

    assert_eq!(profile.id, "openlogi-canonical-synthetic-profile");
    assert_eq!(profile.inventories.len(), 2);
    assert_eq!(profile.inventories[0].paired.len(), 3);
    assert_eq!(profile.standalone.len(), 1);
    assert_eq!(profile.settings.len(), 5);
    assert_eq!(
        profile.inventories[0].receiver.unique_id.as_deref(),
        Some("MOCK-BOLT-01")
    );
    assert!(
        profile
            .inventories
            .iter()
            .flat_map(|inventory| &inventory.paired)
            .filter_map(|device| device.model_info.as_ref())
            .all(|model| model.serial_number.is_none())
    );
    assert!(
        profile
            .standalone
            .iter()
            .all(|device| device.serial_number.is_none())
    );
}

#[test]
fn canonical_device_profile_rejects_unknown_fields_recursively() {
    let canonical: serde_json::Value =
        serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile is JSON");
    let reject = |mut profile: serde_json::Value, pointer: &str, expected_path: &str| {
        profile
            .pointer_mut(pointer)
            .expect("canonical pointer exists")
            .as_object_mut()
            .expect("canonical pointer names an object")
            .insert("unexpected".to_string(), serde_json::Value::Bool(true));
        let error = serde_json::from_value::<DeviceProfile>(profile)
            .expect_err("unknown nested field must be rejected");
        assert!(error.to_string().contains("unknown field"), "{error}");
        assert!(error.to_string().contains(expected_path), "{error}");
    };

    reject(
        canonical.clone(),
        "/standalone/0/address",
        "standalone.0.address.unexpected",
    );
    reject(
        canonical.clone(),
        "/standalone/0/light_capabilities",
        "power",
    );
    reject(
        canonical,
        "/settings/0/smartshift/value",
        "settings.0.smartshift.value.unexpected",
    );
}

#[test]
fn profile_setting_unavailable_is_value_less_and_strict() {
    let unavailable: ProfileSetting<DpiInfo> = serde_json::from_str(r#"{"support":"unavailable"}"#)
        .expect("unavailable setting needs no value");
    assert_eq!(unavailable, ProfileSetting::Unavailable);
    assert_eq!(
        serde_json::to_value(&unavailable).expect("unavailable setting serializes"),
        serde_json::json!({ "support": "unavailable" })
    );

    let error = serde_json::from_str::<ProfileSetting<DpiInfo>>(
        r#"{"support":"unavailable","unexpected":true}"#,
    )
    .expect_err("unknown fields on an unavailable setting must be rejected");
    assert!(error.to_string().contains("unexpected"), "{error}");
}

#[test]
fn schemas_reject_unknown_fields_and_arbitrary_masks() {
    let fixture = direct_probe_fixture();
    let profile = serde_json::to_value(&fixture.profile).expect("profile serializes");
    let reject_profile = |profile, expected_path: &str| {
        let error = serde_json::from_value::<DeviceProfile>(profile)
            .expect_err("unknown profile fields must be rejected at every depth");
        assert!(error.to_string().contains("unknown field"));
        assert!(error.to_string().contains(expected_path));
    };

    let mut unknown_root = profile.clone();
    unknown_root
        .as_object_mut()
        .expect("profile is an object")
        .insert("unexpected".to_string(), serde_json::Value::Bool(true));
    reject_profile(unknown_root, "unexpected");

    let mut unknown_inventory = profile.clone();
    unknown_inventory["inventories"][0]
        .as_object_mut()
        .expect("inventory is an object")
        .insert("unexpected".to_string(), serde_json::Value::Bool(true));
    reject_profile(unknown_inventory, "inventories.0.unexpected");

    let mut unknown_receiver = profile.clone();
    unknown_receiver["inventories"][0]["receiver"]
        .as_object_mut()
        .expect("receiver is an object")
        .insert("unexpected".to_string(), serde_json::Value::Bool(true));
    reject_profile(unknown_receiver, "inventories.0.receiver.unexpected");

    let mut unknown_device = profile;
    unknown_device["inventories"][0]["paired"][0]
        .as_object_mut()
        .expect("paired device is an object")
        .insert("unexpected".to_string(), serde_json::Value::Bool(true));
    reject_profile(unknown_device, "inventories.0.paired.0.unexpected");

    let mut unknown_setting =
        serde_json::to_value(&fixture.profile).expect("profile serializes again");
    unknown_setting["settings"][0]["dpi"]
        .as_object_mut()
        .expect("DPI behavior is an object")
        .insert("unexpected".to_string(), serde_json::Value::Bool(true));
    let setting_error = serde_json::from_value::<DeviceProfile>(unknown_setting)
        .expect_err("unknown setting fields must be rejected");
    assert!(setting_error.to_string().contains("unexpected"));

    let mut cassette = serde_json::to_value(&fixture.cassette).expect("cassette serializes");
    assert_eq!(
        cassette["exchanges"][0]["request"],
        serde_json::Value::String("10ff0010000000".to_string())
    );
    assert_eq!(
        cassette["exchanges"][0]["response"],
        serde_json::Value::String("10ff0010040000".to_string())
    );

    let mut uppercase = cassette.clone();
    uppercase["exchanges"][0]["request"] = serde_json::Value::String("10FF0010000000".to_string());
    let hex_error = serde_json::from_value::<HidCassette>(uppercase)
        .expect_err("cassette reports use canonical lowercase hex");
    assert!(hex_error.to_string().contains("lowercase hexadecimal"));

    cassette["exchanges"][0]
        .as_object_mut()
        .expect("exchange is an object")
        .insert(
            "mask".to_string(),
            serde_json::Value::Array(vec![serde_json::Value::from(0xff)]),
        );
    let cassette_error = serde_json::from_value::<HidCassette>(cassette)
        .expect_err("arbitrary masks are not part of the schema");
    assert!(cassette_error.to_string().contains("unknown field"));
}

#[test]
fn validators_reject_unknown_versions_and_duplicate_slots() {
    let mut fixture = receiver_dpi_fixture();
    fixture.cassette.schema_version += 1;
    assert!(matches!(
        fixture.cassette.validate(),
        Err(FixtureError::UnsupportedSchema { .. })
    ));

    let duplicate = fixture.profile.inventories[0].paired[0].clone();
    fixture.profile.inventories[0].paired.push(duplicate);
    let error = fixture
        .profile
        .validate()
        .expect_err("duplicate receiver slots are ambiguous");
    assert!(error.to_string().contains("repeats slot 1"));
}

#[test]
fn profile_validator_rejects_route_and_setting_inconsistencies() {
    let mut missing_route = direct_probe_fixture().profile;
    missing_route.settings[0].route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xffff,
    };
    let error = missing_route
        .validate()
        .expect_err("setting routes must name an inventory device");
    assert!(error.to_string().contains("does not exist"));

    let mut duplicate = direct_probe_fixture().profile;
    duplicate.settings.push(duplicate.settings[0].clone());
    let error = duplicate
        .validate()
        .expect_err("setting routes must be unique");
    assert!(error.to_string().contains("repeats settings for route"));

    let mut unsupported_current = direct_probe_fixture().profile;
    unsupported_current.settings[0].dpi = ProfileSetting::Supported(DpiInfo {
        current: Dpi::new(900),
        capabilities: DpiCapabilities::new(vec![400, 800]).expect("valid DPI capabilities"),
    });
    let error = unsupported_current
        .validate()
        .expect_err("current DPI must be one of the supported values");
    assert!(
        error
            .to_string()
            .contains("current DPI 900 is not supported")
    );

    let mut inconsistent_capability = direct_probe_fixture().profile;
    inconsistent_capability.settings[0].lighting = ProfileSupport::Supported;
    let error = inconsistent_capability
        .validate()
        .expect_err("setting support must agree with inventory capabilities");
    assert!(
        error
            .to_string()
            .contains("lighting support does not match")
    );

    let mut invalid_backlight = direct_probe_fixture().profile;
    invalid_backlight.settings[0].backlight = ProfileSetting::Supported(BacklightState {
        enabled: true,
        mode: BacklightMode::PermanentManual,
        status: BacklightStatus::PermanentManual,
        current_level: 4,
        nb_levels: 4,
    });
    let error = invalid_backlight
        .validate()
        .expect_err("backlight level must fit its declared range");
    assert!(error.to_string().contains("backlight level 4 exceeds"));
}

#[test]
fn unavailable_settings_count_as_capability_support() {
    let mut profile = direct_probe_fixture().profile;
    profile.settings[0].dpi = ProfileSetting::Unavailable;
    profile.settings[0].wheel = ProfileSetting::Unavailable;
    profile.inventories[0].paired[0]
        .capabilities
        .as_mut()
        .expect("direct fixture has capabilities")
        .hires_wheel = true;
    profile
        .validate()
        .expect("unavailable values still declare present features");

    profile.inventories[0].paired[0]
        .capabilities
        .as_mut()
        .expect("direct fixture has capabilities")
        .pointer = false;
    let error = profile
        .validate()
        .expect_err("unavailable DPI must still match pointer capability");
    assert!(error.to_string().contains("DPI support does not match"));

    let capabilities = profile.inventories[0].paired[0]
        .capabilities
        .as_mut()
        .expect("direct fixture has capabilities");
    capabilities.pointer = true;
    capabilities.hires_wheel = false;
    let error = profile
        .validate()
        .expect_err("unavailable wheel must still match wheel capability");
    assert!(error.to_string().contains("wheel support does not match"));
}

#[test]
fn unavailable_hidpp_settings_remain_invalid_for_standalone_devices() {
    let mut profile: DeviceProfile =
        serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile parses");
    let settings = profile
        .settings
        .iter_mut()
        .find(|settings| matches!(settings.route, DeviceRoute::RawHid { .. }))
        .expect("canonical profile has standalone settings");
    settings.backlight = ProfileSetting::Unavailable;

    let error = profile
        .validate()
        .expect_err("standalone routes cannot expose unavailable HID++ families");
    assert!(error.to_string().contains("declares a HID++ setting"));
}

#[test]
fn unavailable_backlight_still_excludes_rgb_lighting() {
    let mut profile: DeviceProfile =
        serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile parses");
    let settings = profile
        .settings
        .iter_mut()
        .find(|settings| matches!(settings.route, DeviceRoute::Bolt { slot: 3, .. }))
        .expect("canonical profile has RGB keyboard settings");
    settings.backlight = ProfileSetting::Unavailable;

    let error = profile
        .validate()
        .expect_err("present backlight and RGB lighting are mutually exclusive");
    assert!(
        error
            .to_string()
            .contains("cannot expose RGB lighting and backlight together")
    );
}

#[test]
fn profile_validator_rejects_duplicate_identities_and_invalid_ranges() {
    let mut duplicate_identity = receiver_dpi_fixture().profile;
    let mut duplicate_receiver = duplicate_identity.inventories[0].clone();
    duplicate_receiver.paired[0].model_info = None;
    duplicate_identity.inventories.push(duplicate_receiver);
    let error = duplicate_identity
        .validate()
        .expect_err("receiver identities must be unique across inventories");
    assert!(error.to_string().contains("receiver identity"));
    assert!(error.to_string().contains("is repeated"));

    let mut invalid_battery = direct_probe_fixture().profile;
    invalid_battery.inventories[0].paired[0].battery = Some(BatteryInfo {
        percentage: 101,
        level: BatteryLevel::Full,
        status: BatteryStatus::Full,
    });
    let error = invalid_battery
        .validate()
        .expect_err("battery percentages must stay in the semantic range");
    assert!(error.to_string().contains("battery percentage above 100"));
}

#[tokio::test]
async fn hidpp20_rebinds_normal_and_error_responses() {
    let normal_request = short(0xff, 0x00, 0x10, [0, 0, 0]);
    let normal_response = short(0xff, 0x00, 0x10, [4, 0, 0]);
    let error_request = short(0xff, 0x05, 0x20, [0, 0, 0]);
    let error_response = vec![0x10, 0xff, 0xff, 0x05, 0x20, 0x02, 0x00];
    let cassette = cassette(
        "software-id-rebinding",
        DIRECT_CHANNEL,
        vec![
            h20(normal_request, normal_response),
            h20(error_request, error_response),
        ],
    );
    let (raw, handle) =
        ReplayRawHidChannel::new(cassette, 0x046d, 0xb35b).expect("valid replay channel");

    let normal = raw_round_trip(&raw, &[0x10, 0xff, 0x00, 0x1a, 0, 0, 0]).await;
    assert_eq!(normal[3], 0x1a);

    let error = raw_round_trip(&raw, &[0x10, 0xff, 0x05, 0x2e, 0, 0, 0]).await;
    assert_eq!(error[4], 0x2e);
    handle.require_complete().expect("both exchanges consumed");
}

#[tokio::test]
async fn request_keys_allow_receiver_slots_to_interleave_and_repeat_fifo() {
    let slot_one = short(1, 0x05, 0x20, [0, 0, 0]);
    let slot_two = short(2, 0x05, 0x20, [0, 0, 0]);
    let cassette = cassette(
        "interleaved-slots",
        RECEIVER_CHANNEL,
        vec![
            h20(slot_one.clone(), short(1, 0x05, 0x20, [0, 0x01, 0x90])),
            h20(slot_one, short(1, 0x05, 0x20, [0, 0x03, 0x20])),
            h20(slot_two, short(2, 0x05, 0x20, [0, 0x06, 0x40])),
        ],
    );
    let (raw, handle) =
        ReplayRawHidChannel::new(cassette, 0x046d, 0xc548).expect("valid replay channel");

    let slot_two = raw_round_trip(&raw, &[0x10, 2, 0x05, 0x21, 0, 0, 0]).await;
    let slot_one_first = raw_round_trip(&raw, &[0x10, 1, 0x05, 0x22, 0, 0, 0]).await;
    let slot_one_second = raw_round_trip(&raw, &[0x10, 1, 0x05, 0x23, 0, 0, 0]).await;

    assert_eq!(&slot_two[5..7], &[0x06, 0x40]);
    assert_eq!(&slot_one_first[5..7], &[0x01, 0x90]);
    assert_eq!(&slot_one_second[5..7], &[0x03, 0x20]);
    handle
        .require_complete()
        .expect("all slot exchanges consumed");
}

#[tokio::test]
async fn exact_requests_do_not_receive_hidpp20_masking() {
    let cassette = cassette(
        "exact-hidpp10",
        RECEIVER_CHANNEL,
        vec![CassetteExchange {
            request_match: RequestMatch::Exact,
            request: vec![0x10, 0xff, 0x83, 0xfb, 1, 0, 0],
            response: None,
            required: true,
        }],
    );
    let (raw, handle) =
        ReplayRawHidChannel::new(cassette, 0x046d, 0xc548).expect("valid replay channel");

    let error = raw
        .write_report(&[0x10, 0xff, 0x83, 0xfa, 1, 0, 0])
        .await
        .expect_err("HID++ 1.0 register correlation must remain exact");
    assert!(error.to_string().contains("actual=10ff83fa010000"));
    assert!(
        error
            .to_string()
            .contains("hidpp20_normalized=10ff83f0010000")
    );
    let completion = handle.completion();
    assert_eq!(completion.unmatched_requests.len(), 1);
    assert_eq!(completion.unconsumed_required.len(), 1);
}

#[test]
fn completion_rejects_unconsumed_required_exchanges() {
    let cassette = cassette(
        "required",
        DIRECT_CHANNEL,
        vec![h20(
            short(0xff, 0, 0x10, [0, 0, 0]),
            short(0xff, 0, 0x10, [4, 0, 0]),
        )],
    );
    let (_raw, handle) =
        ReplayRawHidChannel::new(cassette, 0x046d, 0xb35b).expect("valid replay channel");
    let error = handle
        .require_complete()
        .expect_err("required exchange was not used");
    assert!(matches!(error, FixtureError::UnconsumedExchanges { .. }));
}

#[tokio::test]
async fn replay_raw_writer_captures_reports_and_models_disconnect() {
    let (mut writer, handle) = ReplayRawWriter::new();
    writer
        .write_output_report(&[0x11, 0x22, 0x33])
        .await
        .expect("connected writer accepts the report");
    assert_eq!(handle.written_reports(), [vec![0x11, 0x22, 0x33]]);

    handle.set_connection(ChannelConnection::Disconnected);
    let error = writer
        .write_output_report(&[0x44])
        .await
        .expect_err("disconnected writer rejects reports");
    assert!(matches!(error, crate::BackendError::Disconnected));
    assert_eq!(handle.written_reports(), [vec![0x11, 0x22, 0x33]]);
}

#[tokio::test]
async fn direct_profile_probe_runs_through_production_enumerator() {
    let fixture = direct_probe_fixture();
    fixture.profile.validate().expect("valid semantic profile");
    let backend = Arc::new(
        ReplayBackend::new(fixture.topology, vec![fixture.cassette])
            .expect("valid direct replay fixture"),
    );
    let mut enumerator = Enumerator::with_backend(backend.clone());

    let inventory = enumerator.enumerate().await.expect("direct probe succeeds");

    assert_eq!(inventory, fixture.profile.inventories);
    assert_eq!(backend.open_count(&fixture.node_id).expect("known node"), 1);
    assert_eq!(
        backend
            .channel_completion(DIRECT_CHANNEL)
            .expect("known channel")
            .channel_open_count,
        1
    );
    backend.require_complete().expect("probe cassette consumed");
}

#[tokio::test]
async fn receiver_backed_dpi_read_runs_through_production_route() {
    let fixture = receiver_dpi_fixture();
    fixture.profile.validate().expect("valid semantic profile");
    let backend = ReplayBackend::new(fixture.topology, vec![fixture.cassette])
        .expect("valid receiver replay fixture");

    let dpi = get_dpi(&backend, &fixture.route)
        .await
        .expect("receiver-backed DPI read succeeds");

    assert_eq!(dpi, Dpi::new(800));
    assert_eq!(backend.open_count(&fixture.node_id).expect("known node"), 1);
    backend.require_complete().expect("DPI cassette consumed");
}

#[tokio::test]
async fn topology_controls_presence_open_connection_slots_and_hotplug_separately() {
    let fixture = receiver_dpi_fixture();
    let backend = ReplayBackend::new(fixture.topology, vec![fixture.cassette])
        .expect("valid receiver replay fixture");
    let mut hotplug = backend.watch().expect("hotplug subscription");

    backend
        .set_node_presence(&fixture.node_id, NodePresence::Absent)
        .expect("known node");
    assert!(
        backend
            .enumerate()
            .await
            .expect("enumeration succeeds")
            .is_empty()
    );
    backend.emit_hotplug(HotplugEvent::Disconnected);
    assert_eq!(hotplug.next().await, Some(HotplugEvent::Disconnected));

    backend
        .set_node_presence(&fixture.node_id, NodePresence::Present)
        .expect("known node");
    backend
        .set_open_outcome(&fixture.node_id, OpenOutcome::Denied)
        .expect("known node");
    let node = backend
        .enumerate()
        .await
        .expect("enumeration succeeds")
        .remove(0);
    let denied = backend.open_hidpp(&node).await;
    assert!(matches!(denied, Err(crate::BackendError::Backend(_))));

    backend
        .set_open_outcome(&fixture.node_id, OpenOutcome::NotHidpp)
        .expect("known node");
    let not_hidpp = backend
        .open_hidpp(&node)
        .await
        .expect("open itself succeeds");
    assert!(not_hidpp.is_none());

    backend
        .set_open_outcome(&fixture.node_id, OpenOutcome::Hidpp)
        .expect("known node");
    let stale = backend
        .open_hidpp(&node)
        .await
        .expect("open succeeds")
        .expect("HID++ channel");
    backend
        .set_channel_connection(fixture.channel, ChannelConnection::Disconnected)
        .expect("known channel");
    assert!(!stale.is_connected());
    backend
        .set_channel_connection(fixture.channel, ChannelConnection::Connected)
        .expect("known channel");
    let replacement = backend
        .open_hidpp(&node)
        .await
        .expect("reopen succeeds")
        .expect("HID++ channel");
    assert!(replacement.is_connected());
    assert!(
        !stale.is_connected(),
        "reconnect must not revive a stale lifetime"
    );

    backend
        .set_receiver_slot_state(
            &fixture.node_id,
            1,
            ReceiverSlotState::Paired(ReceiverLinkState::Offline),
        )
        .expect("known receiver slot");
    assert_eq!(
        backend
            .receiver_slot_state(&fixture.node_id, 1)
            .expect("known receiver slot"),
        ReceiverSlotState::Paired(ReceiverLinkState::Offline)
    );
    assert_eq!(backend.open_count(&fixture.node_id).expect("known node"), 4);
}

async fn raw_round_trip(raw: &ReplayRawHidChannel, request: &[u8]) -> Vec<u8> {
    raw.write_report(request)
        .await
        .expect("request matches cassette");
    let mut response = [0u8; 64];
    let len = raw
        .read_report(&mut response)
        .await
        .expect("cassette queues a response");
    response[..len].to_vec()
}

fn direct_probe_fixture() -> SyntheticFixture {
    let node_id = NodeId::from("synthetic-direct-node".to_string());
    let product_id = 0xb35b;
    let name = "Synthetic Direct Mouse";
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id,
    };
    let profile = DeviceProfile {
        schema_version: FIXTURE_SCHEMA_VERSION,
        id: "direct-mouse-001".to_string(),
        name: name.to_string(),
        inventories: vec![DeviceInventory {
            receiver: ReceiverInfo {
                name: name.to_string(),
                vendor_id: 0x046d,
                product_id,
                unique_id: None,
            },
            paired: vec![PairedDevice {
                slot: crate::DIRECT_DEVICE_INDEX,
                codename: Some(name.to_string()),
                wpid: None,
                kind: DeviceKind::Unknown,
                online: true,
                battery: None,
                model_info: None,
                capabilities: Some(Capabilities {
                    pointer: true,
                    ..Capabilities::default()
                }),
            }],
        }],
        standalone: Vec::new(),
        settings: vec![dpi_settings(route.clone(), 800, vec![400, 800, 1600])],
    };
    let exchanges = vec![
        h20(
            short(0xff, 0x00, 0x10, [0, 0, 0]),
            short(0xff, 0x00, 0x10, [4, 0, 0]),
        ),
        h20(
            short(0xff, 0x00, 0x00, [0x00, 0x01, 0]),
            short(0xff, 0x00, 0x00, [0x01, 0, 0]),
        ),
        h20(
            short(0xff, 0x01, 0x00, [0, 0, 0]),
            short(0xff, 0x01, 0x00, [2, 0, 0]),
        ),
        h20(
            short(0xff, 0x01, 0x10, [1, 0, 0]),
            short(0xff, 0x01, 0x10, [0x00, 0x01, 0]),
        ),
        h20(
            short(0xff, 0x01, 0x10, [2, 0, 0]),
            short(0xff, 0x01, 0x10, [0x22, 0x01, 0]),
        ),
    ];
    SyntheticFixture {
        profile,
        topology: topology(
            node_info(node_id.clone(), product_id, name),
            DIRECT_CHANNEL,
            Vec::new(),
        ),
        cassette: cassette("direct-probe", DIRECT_CHANNEL, exchanges),
        route,
        node_id,
        channel: DIRECT_CHANNEL,
    }
}

fn receiver_dpi_fixture() -> SyntheticFixture {
    let node_id = NodeId::from("synthetic-bolt-node".to_string());
    let product_id = 0xc548;
    let name = "Synthetic Bolt Receiver";
    let mut unique_id_response = vec![0u8; 20];
    unique_id_response[..4].copy_from_slice(&[0x11, 0xff, 0x83, 0xfb]);
    unique_id_response[4..].copy_from_slice(RECEIVER_UID.as_bytes());
    let route = DeviceRoute::Bolt {
        receiver_uid: RECEIVER_UID.to_string(),
        slot: 1,
    };
    let profile = DeviceProfile {
        schema_version: FIXTURE_SCHEMA_VERSION,
        id: "receiver-mouse-001".to_string(),
        name: "Synthetic Receiver Mouse".to_string(),
        inventories: vec![DeviceInventory {
            receiver: ReceiverInfo {
                name: name.to_string(),
                vendor_id: 0x046d,
                product_id,
                unique_id: Some(RECEIVER_UID.to_string()),
            },
            paired: vec![PairedDevice {
                slot: 1,
                codename: Some("Synthetic Receiver Mouse".to_string()),
                wpid: Some(0xb35b),
                kind: DeviceKind::Mouse,
                online: true,
                battery: None,
                model_info: None,
                capabilities: Some(Capabilities {
                    pointer: true,
                    ..Capabilities::default()
                }),
            }],
        }],
        standalone: Vec::new(),
        settings: vec![dpi_settings(route.clone(), 800, vec![400, 800, 1600])],
    };
    let exchanges = vec![
        CassetteExchange {
            request_match: RequestMatch::Exact,
            request: vec![0x10, 0xff, 0x83, 0xfb, 0, 0, 0],
            response: Some(unique_id_response),
            required: true,
        },
        h20(
            short(1, 0x00, 0x10, [0, 0, 0]),
            short(1, 0x00, 0x10, [4, 0, 0]),
        ),
        h20(
            short(1, 0x00, 0x00, [0x22, 0x01, 0]),
            short(1, 0x00, 0x00, [0x05, 0, 0]),
        ),
        h20(
            short(1, 0x05, 0x20, [0, 0, 0]),
            short(1, 0x05, 0x20, [0, 0x03, 0x20]),
        ),
    ];
    SyntheticFixture {
        profile,
        topology: topology(
            node_info(node_id.clone(), product_id, name),
            RECEIVER_CHANNEL,
            vec![ReceiverSlot {
                slot: 1,
                state: ReceiverSlotState::Paired(ReceiverLinkState::Online),
            }],
        ),
        cassette: cassette("receiver-dpi-read", RECEIVER_CHANNEL, exchanges),
        route,
        node_id,
        channel: RECEIVER_CHANNEL,
    }
}

fn dpi_settings(route: DeviceRoute, current: u16, supported: Vec<u16>) -> ProfileDeviceSettings {
    ProfileDeviceSettings {
        route,
        dpi: ProfileSetting::Supported(DpiInfo {
            current: Dpi::new(current),
            capabilities: DpiCapabilities::new(supported)
                .expect("synthetic DPI capabilities are valid"),
        }),
        smartshift: ProfileSetting::Unsupported,
        wheel: ProfileSetting::Unsupported,
        backlight: ProfileSetting::Unsupported,
        lighting: ProfileSupport::Unsupported,
        light: ProfileSupport::Unsupported,
    }
}

fn topology(info: NodeInfo, channel: &str, receiver_slots: Vec<ReceiverSlot>) -> ReplayTopology {
    ReplayTopology {
        nodes: vec![ReplayNode {
            info,
            presence: NodePresence::Present,
            open_outcome: OpenOutcome::Hidpp,
            channel: Some(channel.to_string()),
            raw_writer: RawWriterAvailability::Capture,
            receiver_slots,
        }],
        channels: vec![ReplayChannel {
            id: channel.to_string(),
            connection: ChannelConnection::Connected,
            report_support: ReportSupport::ShortAndLong,
        }],
    }
}

fn node_info(id: NodeId, product_id: u16, name: &str) -> NodeInfo {
    NodeInfo {
        id,
        vendor_id: 0x046d,
        product_id,
        usage_page: 0xff00,
        usage_id: 0x0002,
        name: name.to_string(),
        manufacturer: Some("Logitech".to_string()),
        serial_number: None,
    }
}

fn cassette(name: &str, channel: &str, exchanges: Vec<CassetteExchange>) -> HidCassette {
    HidCassette {
        schema_version: FIXTURE_SCHEMA_VERSION,
        name: name.to_string(),
        channel: channel.to_string(),
        report_support: ReportSupport::ShortAndLong,
        exchanges,
    }
}

fn h20(request: Vec<u8>, response: Vec<u8>) -> CassetteExchange {
    CassetteExchange {
        request_match: RequestMatch::Hidpp20,
        request,
        response: Some(response),
        required: true,
    }
}

fn short(device: u8, feature: u8, function: u8, payload: [u8; 3]) -> Vec<u8> {
    vec![
        0x10, device, feature, function, payload[0], payload[1], payload[2],
    ]
}
