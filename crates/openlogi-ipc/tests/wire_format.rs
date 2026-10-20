//! Golden-bytes guard for the agent↔GUI wire format.
//!
//! The IPC transport serializes with tokio-serde's `Bincode::default()`, which
//! is bincode 1.3 `DefaultOptions` — varint integers, little-endian, reject
//! trailing. (The free functions `bincode::serialize`/`deserialize` use
//! *fixint* encoding and would NOT match the wire — always go through
//! [`bincode::Options`] here.)
//!
//! bincode carries no field names or schema: struct field order, field types,
//! and enum **variant order** are the encoding. These tests pin the exact
//! bytes of every type that crosses the IPC boundary, so a refactor that looks
//! innocent in Rust (reordering variants, retyping a field, wrapping an
//! `Option`) fails here instead of silently corrupting frames across an
//! agent/GUI version skew.
//!
//! If a test fails because you *intended* a wire change, replace the golden
//! with the actual hex from the assertion message. `PROTOCOL_VERSION` moves
//! once per release: bump it and [`protocol_version_is_pinned`] only while it
//! still equals the latest release's value. Otherwise the change joins the
//! unreleased version (`crates/openlogi-ipc/AGENTS.md`).

#![expect(
    clippy::tests_outside_test_module,
    reason = "an integration test file is already its own test-only crate"
)]
#![expect(
    clippy::expect_used,
    reason = "the fixture helpers sit outside any `#[test]` fn, where `allow-expect-in-tests` cannot see them"
)]

use std::collections::BTreeMap;
use std::fmt::Write;

use bincode::Options;
use openlogi_core::app::ForegroundApp;
use openlogi_core::binding::{ActionRingIcon, ActionRingSlot, KeyCombo};
use openlogi_core::config::{ConfigRevision, Lighting, ScrollResolution};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, ChangedOnLight, DeviceInventory,
    DeviceKind, DeviceModelInfo, DeviceTransports, LightCapabilities, LightState, LightValueRange,
    LightValueUnit, LockKey, PairedDevice, RawDeviceAddress, ReceiverInfo, StandaloneDevice,
};
use openlogi_core::hid::{
    BacklightChange, BacklightControls, BacklightMode, BacklightState, BacklightStatus, Click,
    DeviceRoute, Dpi, DpiCapabilities, DpiInfo, EasySwitchHosts, EasySwitchSlot, FnLockState,
    HidppFeatureErrorKind, HidppOperation, HostConnection, HostOsInfo, HostOsKind, LightCommand,
    PasskeyMethod, ReceiverSelector, ReportRate, ReportRateInfo, ScrollReportingTarget,
    ScrollWheelMode, SmartShiftAutoDisengage, SmartShiftMode, SmartShiftStatus,
    SmartShiftThreshold, TunableTorque, WriteError,
};
use openlogi_ipc::{
    ActionRingCommandError, ActionRingInvocation, ActionRingPresentation, AgentRequest,
    AgentSnapshot, AgentStatus, BacklightObservation, ChosenLight, ClientKind, ConfigReloadError,
    ForegroundApps, FoundDevice, Identity, InventoryHealth, LockKeyChange, LockKeyObservation,
    MonitorEvent, Notice, NoticeKind, NoticeObservation, Observation, ObservedLight,
    PROTOCOL_VERSION, PairingCommandError, PairingFailure, PairingPhase, PairingUpdate,
    RingObservation, SavedConfig,
};
use succession::{Compat, Run};

/// Serialize exactly as the transport does (`tokio_serde::formats::Bincode`
/// with its default `O = bincode::DefaultOptions`).
fn wire_bytes<T: serde::Serialize>(value: &T) -> String {
    let bytes = bincode::DefaultOptions::new()
        .serialize(value)
        .expect("wire types serialize");
    bytes.iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    })
}

#[track_caller]
fn assert_wire<T>(value: &T, golden: &str)
where
    T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug,
{
    let hex = wire_bytes(value);
    assert_eq!(
        hex, golden,
        "wire encoding changed — if intentional, regenerate this golden and keep PROTOCOL_VERSION \
         above the latest release's (crates/openlogi-ipc/AGENTS.md)"
    );
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("valid hex"))
        .collect::<Vec<u8>>();
    let decoded: T = bincode::DefaultOptions::new()
        .deserialize(&bytes)
        .expect("wire types deserialize");
    let re_hex = wire_bytes(&decoded);
    assert_eq!(
        hex, re_hex,
        "wire round-trip failed — re-encoded bytes differ"
    );
}

fn representative_smartshift_status() -> SmartShiftStatus {
    SmartShiftStatus {
        mode: SmartShiftMode::Ratchet,
        auto_disengage: SmartShiftAutoDisengage::Threshold(
            SmartShiftThreshold::try_new(16).expect("valid SmartShift threshold"),
        ),
        tunable_torque: Some(TunableTorque::try_new(60).expect("valid SmartShift torque")),
    }
}

/// Pins the version, so the bump that a release's first wire change makes shows
/// in the same diff as its goldens. Later changes before that release join the
/// unreleased version and leave this pin alone (`crates/openlogi-ipc/AGENTS.md`).
#[test]
fn protocol_version_is_pinned() {
    assert_eq!(PROTOCOL_VERSION, 38);
}

#[test]
fn config_reload_result() {
    let error = ConfigReloadError {
        message: "bad".into(),
    };
    assert_wire(&error, "03626164");
    assert_wire(&Ok::<(), ConfigReloadError>(()), "00");
    assert_wire(&Err::<(), ConfigReloadError>(error), "0103626164");
}

/// tarpc encodes the request enum's variant index, so trait *method order* is
/// wire format. `protocol_version` must stay variant 0 forever — it is the
/// cross-version handshake (and the takeover probe) — and new methods append.
#[test]
fn request_variant_order() {
    assert_wire(&AgentRequest::ProtocolVersion {}, "00");
    assert_wire(
        &AgentRequest::SetDpi {
            route: DeviceRoute::Bolt {
                receiver_uid: "F00DCAFE".into(),
                slot: 1,
            },
            dpi: Dpi::new(1600),
        },
        "040008463030444341464501fb4006",
    );
    assert_wire(
        &AgentRequest::SetSmartshift {
            route: DeviceRoute::Bolt {
                receiver_uid: "F00DCAFE".into(),
                slot: 1,
            },
            status: representative_smartshift_status(),
        },
        "06000846303044434146450101103c",
    );
    assert_wire(&AgentRequest::NextPairing {}, "0d");
    assert_wire(&AgentRequest::Snapshot {}, "0e");
    assert_wire(&AgentRequest::PollEventMonitor {}, "0f");
    assert_wire(
        &AgentRequest::SetLight {
            route: DeviceRoute::RawHid {
                vendor_id: 0x046d,
                product_id: 0xc900,
                usage_page: 0xff43,
                usage_id: 0x0202,
                identity: "serial:ABC123".into(),
            },
            command: LightCommand::Power(true),
        },
        "1003fb6d04fb00c9fb43fffb02020d73657269616c3a4142433132330001",
    );
    assert_wire(
        &AgentRequest::SetLightManualPower {
            route: DeviceRoute::RawHid {
                vendor_id: 0x046d,
                product_id: 0xc900,
                usage_page: 0xff43,
                usage_id: 0x0202,
                identity: "serial:ABC123".into(),
            },
            enabled: false,
        },
        "1103fb6d04fb00c9fb43fffb02020d73657269616c3a41424331323300",
    );
    assert_wire(&AgentRequest::NextActionRing {}, "12");
    assert_wire(
        &AgentRequest::ActionRingHover {
            session_id: 42,
            slot: ActionRingSlot::TopRight,
        },
        "132a01",
    );
    assert_wire(
        &AgentRequest::ActionRingActivate {
            session_id: 42,
            slot: ActionRingSlot::TopRight,
        },
        "142a01",
    );
    assert_wire(&AgentRequest::ActionRingCancel { session_id: 42 }, "152a");
    assert_wire(&AgentRequest::Identity {}, "16");
    assert_wire(&AgentRequest::Observe { since: 7 }, "1707");
    assert_wire(&AgentRequest::ObserveActionRing { since: 7 }, "1807");
    assert_wire(
        &AgentRequest::DeclareClient {
            kind: ClientKind::Gui,
        },
        "1900",
    );
    assert_wire(
        &AgentRequest::DeclareClient {
            kind: ClientKind::Cli,
        },
        "1901",
    );
    assert_wire(
        &AgentRequest::DeclareClient {
            kind: ClientKind::Overlay,
        },
        "1902",
    );
}

#[test]
fn semantic_read_requests() {
    assert_wire(
        &AgentRequest::ReadWheel {
            route: DeviceRoute::Bolt {
                receiver_uid: "F00DCAFE".into(),
                slot: 1,
            },
        },
        "1a0008463030444341464501",
    );
    assert_wire(
        &AgentRequest::ReadBacklight {
            route: DeviceRoute::Bolt {
                receiver_uid: "F00DCAFE".into(),
                slot: 1,
            },
        },
        "1b0008463030444341464501",
    );
    assert_wire(
        &AgentRequest::ReadFnLock {
            route: DeviceRoute::Bolt {
                receiver_uid: "F00DCAFE".into(),
                slot: 1,
            },
        },
        "1c0008463030444341464501",
    );
    assert_wire(
        &AgentRequest::SetFnLock {
            route: DeviceRoute::Bolt {
                receiver_uid: "F00DCAFE".into(),
                slot: 1,
            },
            fn_lock: true,
        },
        "1d000846303044434146450101",
    );
    assert_wire(
        &AgentRequest::UnpairDevice {
            route: DeviceRoute::Bolt {
                receiver_uid: "F00DCAFE".into(),
                slot: 1,
            },
        },
        "1e0008463030444341464501",
    );
    assert_wire(
        &AgentRequest::ReadReportRate {
            route: DeviceRoute::Bolt {
                receiver_uid: "F00DCAFE".into(),
                slot: 1,
            },
        },
        "1f0008463030444341464501",
    );
    assert_wire(
        &AgentRequest::SetReportRate {
            route: DeviceRoute::Bolt {
                receiver_uid: "F00DCAFE".into(),
                slot: 1,
            },
            rate: ReportRate::new(1000),
        },
        "200008463030444341464501fbe803",
    );
    assert_wire(
        &AgentRequest::SetBacklight {
            route: DeviceRoute::Bolt {
                receiver_uid: "F00DCAFE".into(),
                slot: 1,
            },
            change: BacklightChange::Level(5),
        },
        "2100084630304443414645010205",
    );
    assert_wire(
        &AgentRequest::ReadEasySwitchHosts {
            route: DeviceRoute::Bolt {
                receiver_uid: "F00DCAFE".into(),
                slot: 1,
            },
        },
        "220008463030444341464501",
    );
    assert_wire(
        &AgentRequest::RenameEasySwitchHost {
            route: DeviceRoute::Bolt {
                receiver_uid: "F00DCAFE".into(),
                slot: 1,
            },
            slot: 1,
            name: "Desk".into(),
        },
        "23000846303044434146450101044465736b",
    );
}

/// After a save, the GUI names the revision of the file it wrote, as the 32
/// bytes of its digest, and the controls of lights the user set without a
/// write to the light.
#[test]
fn reload_saved_config() {
    assert_wire(
        &AgentRequest::ReloadSavedConfig {
            saved: SavedConfig {
                revision: ConfigRevision::of(""),
                chosen: vec![ChosenLight {
                    device: "serial:ABC123".into(),
                    power: false,
                    brightness: true,
                    temperature: true,
                }],
            },
        },
        "24e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\
         010d73657269616c3a414243313233000101",
    );
}

/// The overlay long-polls the backlight indicator by generation, like
/// the other observe calls.
#[test]
fn observe_backlight() {
    assert_wire(&AgentRequest::ObserveBacklight { since: 7 }, "2507");
}

/// The overlay long-polls the notices by generation; a low-battery notice
/// carries the device's name and kind and the reading that made it low.
#[test]
fn notices() {
    assert_wire(&AgentRequest::ObserveNotices { since: 5 }, "2605");
    assert_wire(
        &NoticeObservation {
            generation: 3,
            notices: vec![Notice {
                id: 2,
                kind: NoticeKind::LowBattery {
                    name: Some("MX".into()),
                    kind: DeviceKind::Mouse,
                    battery: BatteryInfo {
                        percentage: Some(15),
                        level: BatteryLevel::Low,
                        status: BatteryStatus::Discharging,
                    },
                },
            }],
            language: Some("de".into()),
        },
        "0301020001024d5800010f010001026465",
    );
    assert_wire(
        &NoticeObservation {
            generation: 1,
            notices: Vec::new(),
            language: None,
        },
        "010000",
    );
}

/// The overlay long-polls the lock-key indicator by generation; it carries
/// the key that changed, whether it is now on, and the configured locale.
#[test]
fn lock_keys() {
    assert_wire(&AgentRequest::ObserveLockKeys { since: 4 }, "2704");
    assert_wire(
        &LockKeyObservation {
            generation: 2,
            shown: Some(LockKeyChange {
                key: LockKey::CapsLock,
                engaged: true,
            }),
            language: Some("de".into()),
        },
        "0201010101026465",
    );
    assert_wire(
        &LockKeyObservation {
            generation: 3,
            shown: Some(LockKeyChange {
                key: LockKey::ScrollLock,
                engaged: false,
            }),
            language: None,
        },
        "0301020000",
    );
}

/// A chord crosses the wire as its modifier bits and the key's HID usage.
#[test]
fn key_combo_modifier_bits() {
    let combo = |text: &str| text.parse::<KeyCombo>().expect("valid chord");
    assert_wire(&combo("Cmd+L"), "010f");
    assert_wire(&combo("Super+L"), "100f");
}

/// The agent identity is frozen: a helper from any build has to be able to
/// decode it, so both halves stay plain `u64`s and this golden never changes.
#[test]
fn agent_identity_is_two_plain_integers() {
    assert_wire(
        &Identity::new(Run::from_raw(7), Compat::from_raw(18)),
        "0712",
    );
}

#[test]
fn action_ring_types() {
    assert_wire(
        &ActionRingInvocation {
            session_id: 42,
            slots: BTreeMap::from([(
                ActionRingSlot::Top,
                ActionRingPresentation {
                    label: "Cut".to_string(),
                    literal: false,
                    icon: ActionRingIcon::Keyboard,
                },
            )]),
            language: Some("fr".to_string()),
        },
        "2a010003437574000701026672",
    );
    // The literal flag is one byte between label and icon.
    assert_wire(
        &ActionRingPresentation {
            label: "Cut".to_string(),
            literal: true,
            icon: ActionRingIcon::Keyboard,
        },
        "034375740107",
    );
    assert_wire(&ActionRingSlot::Top, "00");
    assert_wire(&ActionRingSlot::TopLeft, "07");
    assert_wire(&ActionRingIcon::Keyboard, "07");
    assert_wire(
        &RingObservation {
            generation: 5,
            invocation: None,
        },
        "0500",
    );
    assert_wire(
        &BacklightObservation {
            generation: 7,
            current_level: 3,
            levels: 8,
            visible: true,
        },
        "07030801",
    );
    assert_wire(&ActionRingCommandError::SessionNotFound, "00");
    assert_wire(&ActionRingCommandError::SlotEmpty, "01");
    assert_wire(&HidppOperation::WriteFnLock, "0c");
    assert_wire(&HidppOperation::PlayHaptic, "0e");
    assert_wire(&HidppOperation::ReadFnLock, "0f");
    assert_wire(&HidppOperation::ReadPointerScaling, "10");
    assert_wire(&HidppOperation::WritePointerScaling, "11");
    assert_wire(&HidppOperation::WriteOsLayout, "12");
    assert_wire(&HidppOperation::ReadReportRate, "13");
    assert_wire(&HidppOperation::WriteReportRate, "14");
}

#[test]
fn monitor_events() {
    assert_wire(
        &MonitorEvent::Button {
            button: "Back".into(),
            pressed: true,
        },
        "00044261636b01",
    );
    assert_wire(
        &MonitorEvent::Scroll {
            delta_x: 0.0,
            delta_y: 1.0,
        },
        "01000000000000803f",
    );
    assert_wire(&MonitorEvent::CaptureInterrupted, "02");
}

#[test]
fn agent_status() {
    let status = AgentStatus {
        accessibility_granted: true,
        hook_installed: false,
        launch_at_login: true,
        inventory: InventoryHealth::Ready,
        // A representative value, deliberately not PROTOCOL_VERSION: bumping
        // the version must not churn this golden.
        protocol_version: 7,
        agent_version: "0.6.6".into(),
        input_monitoring_granted: true,
        hid_open_failures: false,
    };
    assert_wire(&status, "010001010705302e362e360100");

    assert_wire(&InventoryHealth::Scanning, "00");
    assert_wire(&InventoryHealth::Ready, "01");
    assert_wire(&InventoryHealth::Unavailable, "02");
}

#[test]
fn agent_snapshot() {
    let snapshot = AgentSnapshot {
        status: AgentStatus {
            accessibility_granted: true,
            hook_installed: false,
            launch_at_login: true,
            inventory: InventoryHealth::Ready,
            protocol_version: 7,
            agent_version: "0.6.6".into(),
            input_monitoring_granted: true,
            hid_open_failures: false,
        },
        inventory: Vec::new(),
        standalone: Vec::new(),
        camera_active: false,
        pairing: None,
        // Pinned on its own in `foreground_apps` below, like the inventory and
        // pairing fields.
        foreground: ForegroundApps::default(),
        // Pinned populated in `observed_light` below.
        lights: Vec::new(),
    };
    assert_wire(&snapshot, "010001010705302e362e36010000000000000000");

    // The observation is the snapshot with its generation in front.
    let observed = Observation {
        generation: 3,
        snapshot,
    };
    assert_wire(&observed, "03010001010705302e362e36010000000000000000");
}

/// The foreground application rides the snapshot, so both halves are pinned:
/// the `None`/empty resting shape and a populated one.
#[test]
fn foreground_apps() {
    assert_wire(&ForegroundApps::default(), "0000");

    let safari = ForegroundApp {
        id: "com.apple.Safari".into(),
        display_name: "Safari".into(),
    };
    assert_wire(&safari, "10636f6d2e6170706c652e53616661726906536166617269");

    assert_wire(
        &ForegroundApps {
            current: Some(safari.clone()),
            recent: vec![safari],
        },
        "0110636f6d2e6170706c652e536166617269065361666172690110636f6d2e6170706c652e53616661726906536166617269",
    );
}

/// The pairing session is state, so its phases are wire format like any enum.
#[test]
fn pairing_phases() {
    assert_wire(&PairingPhase::Searching, "00");
    assert_wire(
        &PairingPhase::Found(vec![FoundDevice {
            address: [1, 2, 3, 4, 5, 6],
            name: "ERGO K860".into(),
        }]),
        "0101010203040506094552474f204b383630",
    );
    assert_wire(&PairingPhase::Pairing, "02");
    assert_wire(&PairingPhase::Paired { slot: 3 }, "0403");
    assert_wire(
        &PairingPhase::Failed(PairingFailure::ReceiverNotFound),
        "0501",
    );
}

#[test]
fn device_inventory() {
    // `Light` was appended after `Unknown`; preserve every existing kind's
    // bincode discriminant.
    assert_wire(&DeviceKind::Light, "0d");
    // A level-only battery (no state-of-charge gauge) carries no percentage.
    assert_wire(
        &BatteryInfo {
            percentage: None,
            level: BatteryLevel::Good,
            status: BatteryStatus::Discharging,
        },
        "000200",
    );
    let inventory = vec![DeviceInventory {
        receiver: ReceiverInfo {
            name: "Bolt Receiver".into(),
            vendor_id: 0x046d,
            product_id: 0xc548,
            unique_id: Some("F00DCAFE".into()),
        },
        paired: vec![PairedDevice {
            slot: 1,
            codename: Some("MX MSTR3S".into()),
            wpid: Some(0xb034),
            kind: DeviceKind::Mouse,
            online: true,
            battery: Some(BatteryInfo {
                percentage: Some(80),
                level: BatteryLevel::Good,
                status: BatteryStatus::Discharging,
            }),
            model_info: Some(DeviceModelInfo {
                entity_count: 3,
                serial_number: Some("2140LZ".into()),
                unit_id: [0x01, 0x02, 0x03, 0x04],
                transports: DeviceTransports {
                    usb: false,
                    equad: true,
                    btle: true,
                    bluetooth: false,
                },
                model_ids: [0xb034, 0x4082, 0],
                extended_model_id: 0x0b,
            }),
            capabilities: Some(Capabilities {
                buttons: true,
                pointer: true,
                lighting: false,
                scroll_inversion: false,
                hires_wheel: true,
                thumbwheel: true,
                haptic_feedback: true,
                haptic_panel: true,
                dpi_gestures: true,
                fn_lock: false,
                os_layout: true,
                report_rate: true,
                backlight: true,
                easy_switch: true,
                virtual_thumbwheel: true,
            }),
            settings_revision: 7,
        }],
    }];
    assert_wire(
        &inventory,
        "010d426f6c74205265636569766572fb6d04fb48c501084630304443414645010101094d58204d535452335301fb34b00001010150020001030106323134304c5a0102030400010100fb34b0fb8240000b0101010000010101010100010101010107",
    );
}

#[test]
fn pairing_updates() {
    assert_wire(&PairingUpdate::Searching, "00");
    assert_wire(
        &PairingUpdate::DeviceFound(FoundDevice {
            address: [0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
            name: "ERGO K860".into(),
        }),
        "01010203040506094552474f204b383630",
    );
    assert_wire(
        &PairingUpdate::Passkey(PasskeyMethod::Keyboard("482913".into())),
        "020006343832393133",
    );
    assert_wire(
        &PairingUpdate::Passkey(PasskeyMethod::Pointer {
            passkey: "12".into(),
            clicks: vec![Click::Left, Click::Right],
        }),
        "0201023132020001",
    );
    assert_wire(&PairingUpdate::Paired { slot: 2 }, "0302");
    assert_wire(&PairingUpdate::Failed(PairingFailure::Timeout), "0403");
    assert_wire(
        &PairingUpdate::Failed(PairingFailure::Device { code: 0x1f }),
        "04041f",
    );
    assert_wire(
        &PairingUpdate::Failed(PairingFailure::UnknownDevice),
        "040b",
    );
    assert_wire(&PairingCommandError::AlreadyActive, "00");
    assert_wire(&PairingCommandError::UnknownDevice, "03");
}

#[test]
fn device_settings_payloads() {
    let dpi: Result<DpiInfo, WriteError> = Ok(DpiInfo {
        current: Dpi::new(1600),
        capabilities: DpiCapabilities::new(vec![800, 1600, 3200]).expect("non-empty list"),
    });
    assert_wire(&dpi, "00fb400603fb2003fb4006fb800c");

    // The GUI matches on this variant to stop re-probing — its index is
    // load-bearing beyond mere decodability.
    let unsupported: Result<DpiInfo, WriteError> = Err(WriteError::FeatureUnsupported {
        feature_hex: 0x2201,
    });
    assert_wire(&unsupported, "0103fb0122");

    assert_wire(
        &WriteError::RequestTimedOut {
            operation: HidppOperation::WriteDpi,
        },
        "0804",
    );
    assert_wire(
        &WriteError::RequestTimedOut {
            operation: HidppOperation::Light,
        },
        "080d",
    );
    assert_wire(
        &WriteError::HidppFeature {
            operation: HidppOperation::WriteDpi,
            feature_hex: 0x2201,
            kind: HidppFeatureErrorKind::OutOfRange,
        },
        "0604fb012203",
    );
    assert_wire(
        &WriteError::UnsupportedResponse {
            operation: HidppOperation::Lighting,
            feature_hex: 0x8070,
        },
        "0707fb7080",
    );
    assert_wire(
        &WriteError::RuntimeInit {
            message: "boom".into(),
        },
        "0904626f6f6d",
    );
    assert_wire(&WriteError::AgentUnavailable, "0a");

    // serde encodes SmartShiftMode's variant *index* (Free=0, Ratchet=1), not
    // the `#[repr(u8)]` firmware discriminants (1/2) — pinned here because it
    // is exactly the kind of thing a refactor would "fix".
    let smartshift: Result<SmartShiftStatus, WriteError> = Ok(representative_smartshift_status());
    assert_wire(&smartshift, "0001103c");

    // `Rgb` serializes as the same hex string the field used to hold raw, so
    // the pinned bytes are identical to the pre-newtype encoding.
    assert_wire(
        &Lighting {
            enabled: true,
            color: "8000ff".parse().expect("valid hex"),
            brightness: 80,
        },
        "010638303030666650",
    );

    assert_wire(
        &ReceiverSelector::BoltUid("F00DCAFE".into()),
        "01084630304443414645",
    );
}

#[test]
fn semantic_read_payloads() {
    assert_wire(&ScrollReportingTarget::Native, "00");
    assert_wire(&ScrollReportingTarget::Diverted, "01");
    let wheel: Result<ScrollWheelMode, WriteError> = Ok(ScrollWheelMode {
        resolution: ScrollResolution::High,
        inverted: false,
        target: ScrollReportingTarget::Native,
    });
    assert_wire(&wheel, "00010000");

    assert_wire(&BacklightMode::PermanentManual, "03");
    assert_wire(&BacklightStatus::PermanentManual, "05");
    let backlight: Result<BacklightState, WriteError> = Ok(BacklightState {
        enabled: true,
        mode: BacklightMode::Automatic,
        status: BacklightStatus::AlsAutomatic,
        current_level: 4,
        nb_levels: 8,
        power_save: true,
        controls: BacklightControls {
            automatic: true,
            level: true,
            power_save: false,
        },
    });
    // v36 appends `power_save` and `controls` after the level count.
    assert_wire(&backlight, "00010102040801010100");

    let fn_lock: Result<FnLockState, WriteError> = Ok(FnLockState {
        fn_lock: true,
        default_fn_lock: Some(false),
    });
    assert_wire(&fn_lock, "00010100");
    // A `0x40a0` keyboard reports no default.
    let legacy: Result<FnLockState, WriteError> = Ok(FnLockState {
        fn_lock: true,
        default_fn_lock: None,
    });
    assert_wire(&legacy, "000100");
}

/// A light the agent reads over HID++, with a brightness limit and settings
/// changed on the light.
#[test]
fn observed_light() {
    let brightness =
        LightValueRange::new(20, 250, 1, LightValueUnit::Lumens).expect("valid brightness range");
    let observed = ObservedLight {
        route: DeviceRoute::RawHid {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:glow-1".into(),
        },
        state: LightState {
            capabilities: LightCapabilities {
                power: true,
                brightness: Some(brightness),
                temperature: None,
                color: false,
                zones: false,
            },
            on: true,
            brightness: Some(150),
            temperature_kelvin: None,
            brightness_limit: Some(200),
        },
        changed_on_light: ChangedOnLight {
            enabled: None,
            brightness_percent: Some(57),
            temperature_kelvin: Some(4000),
        },
    };
    assert_wire(
        &observed,
        "03fb6d04fb00c9fb43fffb02020d73657269616c3a676c6f772d31010114fa0101000000\
         0101960001c800013901fba00f",
    );
}

#[test]
fn standalone_light_dtos_commands_and_errors() {
    let brightness =
        LightValueRange::new(20, 250, 1, LightValueUnit::Lumens).expect("valid brightness range");
    let temperature = LightValueRange::new(2700, 6500, 100, LightValueUnit::Kelvin)
        .expect("valid temperature range");
    let capabilities = LightCapabilities {
        power: true,
        brightness: Some(brightness),
        temperature: Some(temperature),
        color: false,
        zones: false,
    };
    let standalone = StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:glow-1".into(),
        },
        display_name: "Litra Glow".into(),
        manufacturer: Some("Logi".into()),
        serial_number: Some("glow-1".into()),
        unit_id: [0; 4],
        kind: DeviceKind::Light,
        online: true,
        capabilities: None,
        light_capabilities: Some(capabilities),
        driver_id: "litra".into(),
        registry_model_id: Some("8c900".into()),
    };

    assert_wire(
        &standalone,
        "fb6d04fb00c9fb43fffb02020d73657269616c3a676c6f772d310a4c6974726120476c6f7701044c6f67690106676c6f772d31000000000d010001010114fa010101fb8c0afb641964020000056c6974726101053863393030",
    );
    let mut legacy = standalone.clone();
    legacy.registry_model_id = None;
    assert_wire(
        &legacy,
        "fb6d04fb00c9fb43fffb02020d73657269616c3a676c6f772d310a4c6974726120476c6f7701044c6f67690106676c6f772d31000000000d010001010114fa010101fb8c0afb641964020000056c6974726100",
    );
    assert_wire(&capabilities, "010114fa010101fb8c0afb641964020000");
    assert_wire(&brightness, "14fa0101");
    assert_wire(&temperature, "fb8c0afb64196402");
    assert_wire(&LightCommand::Power(true), "0001");
    assert_wire(&LightCommand::BrightnessPercent(65), "0141");
    assert_wire(&LightCommand::TemperatureKelvin(4600), "02fbf811");
    assert_wire(&LightCommand::BrightnessNative(136), "0388");
    assert_wire(
        &WriteError::InvalidLightValue {
            control: "temperature_kelvin".into(),
            value: 2750,
        },
        "0b1274656d70657261747572655f6b656c76696efbbe0a",
    );
    assert_wire(
        &WriteError::LightUnsupported {
            control: "color".into(),
        },
        "0c05636f6c6f72",
    );
    assert_wire(&WriteError::AmbiguousRawDevice, "0d");
    assert_wire(
        &WriteError::DpiUnsupported {
            requested: Dpi::new(1000),
        },
        "0efbe803",
    );
    assert_wire(
        &WriteError::DpiNotApplied {
            requested: Dpi::new(1600),
            actual: Dpi::new(800),
        },
        "0ffb4006fb2003",
    );
    assert_wire(
        &WriteError::HapticWaveformUnsupported { waveform: 1 },
        "1001",
    );
    assert_wire(&WriteError::HapticsUnavailable { status: 2 }, "1502");
}

#[test]
fn easy_switch_hosts_and_errors() {
    assert_wire(
        &EasySwitchHosts {
            current: Some(1),
            slots: vec![
                EasySwitchSlot {
                    index: 0,
                    paired: true,
                    connection: HostConnection::Bluetooth,
                    name: Some("Desk".into()),
                    name_max_len: 24,
                    os: Some(HostOsInfo {
                        kind: HostOsKind::MacOs,
                        major: 15,
                        minor: 2,
                        patch: 3,
                    }),
                },
                EasySwitchSlot {
                    index: 1,
                    paired: false,
                    connection: HostConnection::Unknown,
                    name: None,
                    name_max_len: 24,
                    os: None,
                },
            ],
            can_rename: true,
        },
        "01010200010301044465736b1801050f020301000000180001",
    );
    assert_wire(&HidppOperation::ReadHosts, "15");
    assert_wire(&HidppOperation::RenameHost, "16");
    assert_wire(&WriteError::HostRenameUnsupported, "16");
    assert_wire(&WriteError::HostSlotEmpty { slot: 2 }, "1702");
    assert_wire(&WriteError::HostNameRejected { max_len: 24 }, "1818");
    assert_wire(
        &WriteError::HostNameNotApplied {
            requested: "Desk".into(),
            actual: Some("Old".into()),
        },
        "19044465736b01034f6c64",
    );
}

#[test]
fn backlight_changes_and_errors() {
    assert_wire(&BacklightChange::Enabled(true), "0001");
    assert_wire(&BacklightChange::Automatic(false), "0100");
    assert_wire(&BacklightChange::Level(3), "0203");
    assert_wire(&BacklightChange::PowerSave(true), "0301");
    assert_wire(
        &WriteError::BacklightChangeUnsupported {
            change: BacklightChange::Level(8),
        },
        "130208",
    );
    assert_wire(
        &WriteError::BacklightNotApplied {
            change: BacklightChange::Automatic(true),
        },
        "140101",
    );
}

#[test]
fn report_rate_dtos_and_errors() {
    assert_wire(
        &WriteError::ReportRateUnsupported {
            requested: ReportRate::new(2000),
        },
        "11fbd007",
    );
    assert_wire(
        &WriteError::ReportRateNotApplied {
            requested: ReportRate::new(1000),
            actual: ReportRate::new(500),
        },
        "12fbe803fbf401",
    );
    // Rates are whole hertz as varints: 125 fits one byte, the rest take three.
    assert_wire(
        &ReportRateInfo {
            current: ReportRate::new(1000),
            supported: vec![
                ReportRate::new(125),
                ReportRate::new(500),
                ReportRate::new(1000),
            ],
        },
        "fbe803037dfbf401fbe803",
    );
}
