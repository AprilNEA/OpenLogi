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
//! If a test fails because you *intended* a wire change: bump
//! `PROTOCOL_VERSION`, update [`protocol_version_is_pinned`], and replace the
//! golden with the actual hex from the assertion message.

#![allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]

use std::fmt::Write;

use bincode::Options;
use openlogi_agent_core::ipc::{
    AgentRequest, AgentStatus, FoundDevice, InventoryHealth, PROTOCOL_VERSION, PairingUpdate,
};
use openlogi_core::config::Lighting;
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, DeviceInventory, DeviceKind,
    DeviceModelInfo, DeviceTransports, PairedDevice, ReceiverInfo,
};
use openlogi_hid::{
    Click, DeviceRoute, DpiCapabilities, DpiInfo, PasskeyMethod, ReceiverSelector, SmartShiftMode,
    SmartShiftStatus, WriteError,
};

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
fn assert_wire<T: serde::Serialize>(value: &T, golden: &str) {
    assert_eq!(
        wire_bytes(value),
        golden,
        "wire encoding changed — if intentional, bump PROTOCOL_VERSION and regenerate this golden"
    );
}

/// Any golden regeneration must come with a version bump — this is the test
/// that makes that visible in the same diff.
#[test]
fn protocol_version_is_pinned() {
    assert_eq!(PROTOCOL_VERSION, 3);
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
            dpi: 1600,
        },
        "040008463030444341464501fb4006",
    );
    assert_wire(&AgentRequest::NextPairing {}, "0d");
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
    };
    assert_wire(&status, "010001010705302e362e36");

    assert_wire(&InventoryHealth::Scanning, "00");
    assert_wire(&InventoryHealth::Ready, "01");
    assert_wire(&InventoryHealth::Unavailable, "02");
}

#[test]
fn device_inventory() {
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
                percentage: 80,
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
            }),
        }],
    }];
    assert_wire(
        &inventory,
        "010d426f6c74205265636569766572fb6d04fb48c501084630304443414645010101094d58204d535452335301fb34b000010150020001030106323134304c5a0102030400010100fb34b0fb8240000b01010100",
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
    assert_wire(
        &PairingUpdate::Failed("timed out".into()),
        "040974696d6564206f7574",
    );
}

#[test]
fn device_settings_payloads() {
    let dpi: Result<DpiInfo, WriteError> = Ok(DpiInfo {
        current: 1600,
        capabilities: DpiCapabilities::new(vec![800, 1600, 3200]).expect("non-empty list"),
    });
    assert_wire(&dpi, "00fb400603fb2003fb4006fb800c");

    // The GUI matches on this variant to stop re-probing — its index is
    // load-bearing beyond mere decodability.
    let unsupported: Result<DpiInfo, WriteError> = Err(WriteError::FeatureUnsupported {
        feature_hex: 0x2201,
    });
    assert_wire(&unsupported, "0103fb0122");

    // serde encodes SmartShiftMode's variant *index* (Free=0, Ratchet=1), not
    // the `#[repr(u8)]` firmware discriminants (1/2) — pinned here because it
    // is exactly the kind of thing a refactor would "fix".
    let smartshift: Result<SmartShiftStatus, WriteError> = Ok(SmartShiftStatus {
        mode: SmartShiftMode::Ratchet,
        auto_disengage: 16,
        tunable_torque: 60,
    });
    assert_wire(&smartshift, "0001103c");

    assert_wire(
        &Lighting {
            enabled: true,
            color: "8000ff".into(),
            brightness: 80,
        },
        "010638303030666650",
    );

    assert_wire(
        &ReceiverSelector::BoltUid("F00DCAFE".into()),
        "01084630304443414645",
    );
}
