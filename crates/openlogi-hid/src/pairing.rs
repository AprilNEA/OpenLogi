//! Wireless device pairing for Logi Bolt and Unifying receivers.
//!
//! The published `hidpp 0.2` can only *read* existing pairings, and its
//! `BoltReceiver` is closed to extension. So OpenLogi drives the receiver's
//! HID++ 1.0 registers directly over the public [`HidppChannel`] primitives,
//! the same way [`crate::write`] and [`crate::gesture`] bypass the crate's
//! higher-level abstractions.
//!
//! The register layout and notification framing below are reverse engineered
//! from Solaar (the authoritative open-source reference) and cross-checked
//! against `hidpp 0.2`'s own `0x41` device-connection parser. Two families,
//! two flows:
//!
//! - **Bolt** (`046d:c548`): open *discovery* → the receiver streams nearby
//!   unpaired devices → pick one → pair by its BTLE address → the device
//!   shows a *passkey* the user types (keyboard) or clicks (pointer) →
//!   success carries the assigned slot.
//! - **Unifying** (`046d:c52b`, `046d:c532`): open a pairing *lock*; the next
//!   powered-on unpaired device in range links on its own. No discovery list,
//!   no passkey.
//!
//! Drive a session with [`run_pairing`]: it streams [`PairingEvent`]s out and
//! takes [`PairingCommand`]s in (the Bolt device pick / cancel). [`unpair`]
//! removes a slot; [`list_pairing_receivers`] reports what's connectable.

use std::{collections::HashMap, sync::Arc};

use hidpp::{
    channel::{HidppChannel, HidppMessage},
    receiver::{self, Receiver},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

pub use hidpp::receiver::bolt::DeviceKind as BoltDeviceKind;

use crate::transport::{enumerate_hidpp_devices, open_hidpp_channel};

/// HID++ device index addressing the receiver itself (not a paired device).
const RECEIVER_INDEX: u8 = 0xff;

/// Receiver registers (HID++ 1.0 RAP).
mod reg {
    /// Notification-flags register (3-byte big-endian value).
    pub const NOTIFICATIONS: u8 = 0x00;
    /// Unifying pairing lock + unpair.
    pub const UNIFYING_PAIRING: u8 = 0xb2;
    /// Bolt discovery start/stop (short register).
    pub const BOLT_DISCOVERY: u8 = 0xc0;
    /// Bolt pair / cancel / unpair (long register).
    pub const BOLT_PAIRING: u8 = 0xc1;
}

/// Notification sub-IDs the receiver emits during pairing.
mod notif {
    pub const DEVICE_CONNECTION: u8 = 0x41;
    pub const UNIFYING_LOCK: u8 = 0x4a;
    pub const PASSKEY_REQUEST: u8 = 0x4d;
    pub const DEVICE_DISCOVERY: u8 = 0x4f;
    pub const DISCOVERY_STATUS: u8 = 0x53;
    pub const PAIRING_STATUS: u8 = 0x54;
}

/// `WIRELESS` (0x000100) | `SOFTWARE_PRESENT` (0x000800) notification flags,
/// big-endian. Both must be set for the receiver to stream pairing events.
const NOTIF_FLAGS: [u8; 3] = [0x00, 0x09, 0x00];

/// Product IDs (vendor `0x046d`) of supported pairing-capable receivers.
const BOLT_PRODUCT_IDS: &[u16] = &[0xc548];
const UNIFYING_PRODUCT_IDS: &[u16] = &[0xc52b, 0xc532];

/// Receiver pairing family. Each uses a different register flow.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReceiverFamily {
    Bolt,
    Unifying,
}

fn family_for(product_id: u16) -> Option<ReceiverFamily> {
    if BOLT_PRODUCT_IDS.contains(&product_id) {
        Some(ReceiverFamily::Bolt)
    } else if UNIFYING_PRODUCT_IDS.contains(&product_id) {
        Some(ReceiverFamily::Unifying)
    } else {
        None
    }
}

/// A pairing-capable receiver currently connected to the host.
#[derive(Clone, Debug)]
pub struct PairingReceiver {
    /// Bolt unique ID, when readable. `None` for Unifying (no read path yet).
    pub uid: Option<String>,
    pub family: ReceiverFamily,
    pub product_id: u16,
}

/// Selects which receiver a pairing operation targets.
///
/// Crosses the agent↔GUI IPC (`start_pairing`), so variant order is wire
/// format — changes require a `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-agent-core/tests/wire_format.rs`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ReceiverSelector {
    /// The first supported receiver found — fine for the common single-receiver case.
    First,
    /// A specific Bolt receiver by its unique ID.
    BoltUid(String),
}

/// A nearby unpaired device surfaced by Bolt discovery.
#[derive(Clone, Debug)]
pub struct DiscoveredDevice {
    /// 6-byte BTLE address used to pair.
    pub address: [u8; 6],
    /// Authentication-method bitfield (bit 0 = passkey typed on keyboard).
    pub authentication: u8,
    pub kind: BoltDeviceKind,
    pub name: String,
}

impl DiscoveredDevice {
    /// Whether authentication is by typing a passkey on a keyboard (vs. a
    /// pointer click sequence).
    #[must_use]
    pub fn passkey_on_keyboard(&self) -> bool {
        self.authentication & 0x01 != 0
    }

    /// Pairing entropy: keyboards use 20 bits, everything else 10.
    fn entropy(&self) -> u8 {
        if self.kind == BoltDeviceKind::Keyboard {
            20
        } else {
            10
        }
    }
}

/// A single click in a pointer passkey sequence.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Click {
    Left,
    Right,
}

/// How the user authenticates the device during Bolt pairing.
///
/// Crosses the agent↔GUI IPC (inside `PairingUpdate::Passkey`, [`Click`]
/// included), so variant and field order are wire format — changes require a
/// `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-agent-core/tests/wire_format.rs`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PasskeyMethod {
    /// Type these digits on the new keyboard, then press Enter.
    Keyboard(String),
    /// On the new pointer, perform this left/right click sequence, then click
    /// both buttons together.
    Pointer { passkey: String, clicks: Vec<Click> },
}

/// Renders a Bolt passkey as a 10-bit MSB-first left/right click sequence.
fn passkey_to_clicks(passkey: &str) -> Vec<Click> {
    let value: u32 = passkey.trim().parse().unwrap_or(0);
    (0..10)
        .rev()
        .map(|bit| {
            if value & (1 << bit) != 0 {
                Click::Right
            } else {
                Click::Left
            }
        })
        .collect()
}

/// Events streamed out of a pairing session.
#[derive(Clone, Debug)]
pub enum PairingEvent {
    /// Discovery (Bolt) or the pairing lock (Unifying) is now open.
    Searching,
    /// Bolt only: a nearby unpaired device was discovered.
    DeviceFound(DiscoveredDevice),
    /// Bolt only: the device asks the user to enter a passkey to authenticate.
    Passkey(PasskeyMethod),
    /// A device was paired and assigned `slot`.
    Paired { slot: u8 },
    /// The flow ended without pairing a device.
    Failed(PairingError),
}

/// Commands fed into a pairing session.
#[derive(Clone, Debug)]
pub enum PairingCommand {
    /// Bolt: pair with a previously discovered device.
    Pair(DiscoveredDevice),
    /// Abort the in-progress flow.
    Cancel,
}

/// Errors raised by pairing operations.
#[derive(Clone, Debug, Error)]
pub enum PairingError {
    #[error("HID transport error: {0}")]
    Hid(String),
    #[error("no supported pairing-capable receiver found")]
    ReceiverNotFound,
    #[error("receiver register access failed: {0}")]
    Register(String),
    #[error("pairing timed out")]
    Timeout,
    #[error("receiver reported pairing error {0:#04x}")]
    Device(u8),
    #[error("pairing was cancelled")]
    Cancelled,
}

impl From<async_hid::HidError> for PairingError {
    fn from(e: async_hid::HidError) -> Self {
        PairingError::Hid(e.to_string())
    }
}

/// Lists supported pairing-capable receivers connected to the host.
pub async fn list_pairing_receivers() -> Result<Vec<PairingReceiver>, PairingError> {
    let mut out = Vec::new();
    for dev in enumerate_hidpp_devices().await? {
        let Some((_, channel)) = open_hidpp_channel(dev).await? else {
            continue;
        };
        let Some(family) = family_for(channel.product_id) else {
            continue;
        };
        let uid = match family {
            ReceiverFamily::Bolt => read_bolt_uid(&channel).await,
            ReceiverFamily::Unifying => None,
        };
        out.push(PairingReceiver {
            uid,
            family,
            product_id: channel.product_id,
        });
    }
    Ok(out)
}

/// Reads a Bolt receiver's unique ID via the crate's `BoltReceiver`.
async fn read_bolt_uid(channel: &Arc<HidppChannel>) -> Option<String> {
    let Some(Receiver::Bolt(bolt)) = receiver::detect(Arc::clone(channel)) else {
        return None;
    };
    bolt.get_unique_id().await.ok()
}

/// Opens the channel for the receiver named by `target`.
async fn open_receiver(
    target: &ReceiverSelector,
) -> Result<(Arc<HidppChannel>, ReceiverFamily), PairingError> {
    for dev in enumerate_hidpp_devices().await? {
        let Some((_, channel)) = open_hidpp_channel(dev).await? else {
            continue;
        };
        let Some(family) = family_for(channel.product_id) else {
            continue;
        };
        match target {
            ReceiverSelector::First => return Ok((channel, family)),
            ReceiverSelector::BoltUid(want) => {
                if family == ReceiverFamily::Bolt
                    && read_bolt_uid(&channel)
                        .await
                        .is_some_and(|uid| uid.eq_ignore_ascii_case(want))
                {
                    return Ok((channel, family));
                }
            }
        }
    }
    Err(PairingError::ReceiverNotFound)
}

/// Decodes a raw HID++ message into `(device_index, sub_id, payload)`, where
/// `payload[0]` is the HID++ 1.0 notification *address* byte and `payload[k]`
/// for `k >= 1` is Solaar's `data[k - 1]`. Short payloads are zero-padded.
fn decode(msg: &HidppMessage) -> (u8, u8, [u8; 17]) {
    let mut payload = [0u8; 17];
    match msg {
        HidppMessage::Short(d) => {
            payload[..4].copy_from_slice(&d[2..6]);
            (d[0], d[1], payload)
        }
        HidppMessage::Long(d) => {
            payload.copy_from_slice(&d[2..19]);
            (d[0], d[1], payload)
        }
    }
}

/// A parsed receiver notification relevant to pairing.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Notification {
    /// Bolt discovery address frame: kind, BTLE address, auth method.
    DiscoveryInfo {
        counter: u16,
        kind: u8,
        address: [u8; 6],
        authentication: u8,
    },
    /// Bolt discovery name frame.
    DiscoveryName { counter: u16, name: String },
    /// Bolt pairing completed; `slot` is the assigned device index.
    PairingSucceeded { slot: u8 },
    /// Bolt pairing/discovery failed with a receiver error code.
    PairingError(u8),
    /// Bolt passkey to present to the user (6 ASCII digits).
    Passkey(String),
    /// A device linked to the receiver (`slot` = its device index).
    Connected { slot: u8, established: bool },
    /// Unifying pairing lock changed state; `error` is non-zero on failure.
    UnifyingLock { open: bool, error: u8 },
}

/// Parses a raw message into a pairing [`Notification`], if it is one.
fn parse_notification(sub_id: u8, device_index: u8, p: [u8; 17]) -> Option<Notification> {
    match sub_id {
        notif::DEVICE_CONNECTION => Some(Notification::Connected {
            slot: device_index,
            // bit 6 of the flags byte set => link not established (offline).
            established: p[1] & (1 << 6) == 0,
        }),
        notif::DEVICE_DISCOVERY => {
            let counter = u16::from(p[0]) + u16::from(p[1]) * 256;
            match p[2] {
                0 => {
                    let mut address = [0u8; 6];
                    address.copy_from_slice(&p[7..13]);
                    Some(Notification::DiscoveryInfo {
                        counter,
                        kind: p[4],
                        address,
                        authentication: p[15],
                    })
                }
                1 => {
                    let len = usize::from(p[3]).min(p.len() - 4);
                    let name = String::from_utf8_lossy(&p[4..4 + len]).into_owned();
                    Some(Notification::DiscoveryName { counter, name })
                }
                _ => None,
            }
        }
        notif::DISCOVERY_STATUS => {
            let error = p[1];
            if error != 0 {
                Some(Notification::PairingError(error))
            } else {
                None
            }
        }
        notif::PAIRING_STATUS => {
            let error = p[1];
            if error != 0 {
                Some(Notification::PairingError(error))
            } else if p[0] == 0x02 {
                // address 0x02 with no error => paired; slot is data[7] = p[8].
                Some(Notification::PairingSucceeded { slot: p[8] })
            } else {
                None
            }
        }
        notif::PASSKEY_REQUEST => {
            let passkey = String::from_utf8_lossy(&p[1..7]).into_owned();
            Some(Notification::Passkey(passkey))
        }
        notif::UNIFYING_LOCK => Some(Notification::UnifyingLock {
            open: p[0] & 0x01 != 0,
            error: p[1],
        }),
        _ => None,
    }
}

/// Subscribes a listener that forwards unmatched messages to an async channel,
/// and returns the listener handle plus the receiver end.
fn subscribe(channel: &HidppChannel) -> (u32, mpsc::UnboundedReceiver<HidppMessage>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let hdl = channel.add_msg_listener(move |msg, matched| {
        // `matched` messages are responses to our own register writes.
        if !matched {
            let _ = tx.send(msg);
        }
    });
    (hdl, rx)
}

/// Overall guard so a wedged receiver can't hang the session forever.
const SESSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
/// Discovery / lock window opened on the receiver, in seconds.
const DISCOVERY_TIMEOUT: u8 = 30;

/// Runs a pairing session against `target`, streaming [`PairingEvent`]s to
/// `events` and consuming [`PairingCommand`]s from `commands`. Returns when the
/// flow finishes (paired, failed, cancelled, or timed out).
///
/// The caller owns the orchestration: spawn this on a runtime, hold the command
/// sender to forward the user's device pick / cancel, and read events to drive
/// the UI.
pub async fn run_pairing(
    target: ReceiverSelector,
    mut commands: mpsc::UnboundedReceiver<PairingCommand>,
    events: mpsc::UnboundedSender<PairingEvent>,
) -> Result<(), PairingError> {
    let (channel, family) = open_receiver(&target).await?;
    let (listener, mut notifications) = subscribe(&channel);

    let result = drive(&channel, family, &mut commands, &mut notifications, &events).await;

    channel.remove_msg_listener(listener);
    // Best-effort restore: clear notification flags we set.
    let _ = channel
        .write_register(RECEIVER_INDEX, reg::NOTIFICATIONS, [0, 0, 0])
        .await;

    if let Err(ref e) = result {
        let _ = events.send(PairingEvent::Failed(e.clone()));
    }
    result
}

/// Core session loop. Split out so [`run_pairing`] can always run teardown.
async fn drive(
    channel: &HidppChannel,
    family: ReceiverFamily,
    commands: &mut mpsc::UnboundedReceiver<PairingCommand>,
    notifications: &mut mpsc::UnboundedReceiver<HidppMessage>,
    events: &mpsc::UnboundedSender<PairingEvent>,
) -> Result<(), PairingError> {
    write_register(channel, reg::NOTIFICATIONS, NOTIF_FLAGS).await?;

    match family {
        ReceiverFamily::Bolt => {
            write_register(
                channel,
                reg::BOLT_DISCOVERY,
                [DISCOVERY_TIMEOUT, 0x01, 0x00],
            )
            .await?;
        }
        ReceiverFamily::Unifying => {
            write_register(
                channel,
                reg::UNIFYING_PAIRING,
                [0x01, 0x00, DISCOVERY_TIMEOUT],
            )
            .await?;
        }
    }
    let _ = events.send(PairingEvent::Searching);

    // Partial Bolt discovery frames, keyed by discovery counter.
    let mut partial: HashMap<u16, PartialDevice> = HashMap::new();
    // Auth byte of the device the user chose to pair, for passkey rendering.
    let mut pairing_auth: Option<u8> = None;
    let deadline = tokio::time::sleep(SESSION_TIMEOUT);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            () = &mut deadline => return Err(PairingError::Timeout),

            cmd = commands.recv() => match cmd {
                Some(PairingCommand::Pair(device)) => {
                    pairing_auth = Some(device.authentication);
                    pair_bolt_device(channel, &device).await?;
                }
                Some(PairingCommand::Cancel) | None => {
                    cancel(channel, family).await;
                    return Err(PairingError::Cancelled);
                }
            },

            msg = notifications.recv() => {
                let Some(msg) = msg else {
                    return Err(PairingError::Hid("receiver channel closed".into()));
                };
                let (device_index, sub_id, payload) = decode(&msg);
                // Reverse-engineered wire format — log every notification so a
                // mis-parse can be diagnosed against real hardware.
                trace!(sub_id = format_args!("{sub_id:#04x}"), ?payload, "pairing notification");
                let Some(note) = parse_notification(sub_id, device_index, payload) else {
                    continue;
                };
                match note {
                    Notification::DiscoveryInfo { counter, kind, address, authentication } => {
                        let entry = partial.entry(counter).or_default();
                        entry.kind = Some(kind);
                        entry.address = Some(address);
                        entry.authentication = Some(authentication);
                        if let Some(device) = entry.build() {
                            let _ = events.send(PairingEvent::DeviceFound(device));
                        }
                    }
                    Notification::DiscoveryName { counter, name } => {
                        let entry = partial.entry(counter).or_default();
                        entry.name = Some(name);
                        if let Some(device) = entry.build() {
                            let _ = events.send(PairingEvent::DeviceFound(device));
                        }
                    }
                    Notification::Passkey(passkey) => {
                        let method = match pairing_auth {
                            Some(auth) if auth & 0x01 != 0 => PasskeyMethod::Keyboard(passkey),
                            _ => PasskeyMethod::Pointer {
                                clicks: passkey_to_clicks(&passkey),
                                passkey,
                            },
                        };
                        let _ = events.send(PairingEvent::Passkey(method));
                    }
                    Notification::PairingSucceeded { slot } => {
                        let _ = events.send(PairingEvent::Paired { slot });
                        return Ok(());
                    }
                    Notification::PairingError(code) => return Err(PairingError::Device(code)),
                    Notification::Connected { slot, established } if family == ReceiverFamily::Unifying => {
                        if established {
                            let _ = events.send(PairingEvent::Paired { slot });
                            return Ok(());
                        }
                    }
                    Notification::Connected { .. } => {}
                    Notification::UnifyingLock { open, error } => {
                        if error != 0 {
                            return Err(PairingError::Device(error));
                        }
                        if !open {
                            // Lock closed without a connection notification: nothing paired.
                            return Err(PairingError::Timeout);
                        }
                    }
                }
            }
        }
    }
}

/// Accumulates the two Bolt discovery frames for one device.
#[derive(Default)]
struct PartialDevice {
    kind: Option<u8>,
    address: Option<[u8; 6]>,
    authentication: Option<u8>,
    name: Option<String>,
    emitted: bool,
}

impl PartialDevice {
    /// Builds a [`DiscoveredDevice`] once both frames have arrived, exactly once.
    fn build(&mut self) -> Option<DiscoveredDevice> {
        if self.emitted {
            return None;
        }
        let (kind, address, authentication, name) = (
            self.kind?,
            self.address?,
            self.authentication?,
            self.name.clone()?,
        );
        self.emitted = true;
        Some(DiscoveredDevice {
            address,
            authentication,
            kind: BoltDeviceKind::try_from(kind & 0x0f).unwrap_or(BoltDeviceKind::Unknown),
            name,
        })
    }
}

/// Sends the Bolt pair command (action `0x01`, auto slot) for `device`.
async fn pair_bolt_device(
    channel: &HidppChannel,
    device: &DiscoveredDevice,
) -> Result<(), PairingError> {
    let mut payload = [0u8; 16];
    payload[0] = 0x01; // action: pair
    payload[1] = 0x00; // slot: auto-assign
    payload[2..8].copy_from_slice(&device.address);
    payload[8] = device.authentication;
    payload[9] = device.entropy();
    write_long_register(channel, reg::BOLT_PAIRING, payload).await
}

/// Best-effort cancel of an in-progress flow.
async fn cancel(channel: &HidppChannel, family: ReceiverFamily) {
    let res = match family {
        ReceiverFamily::Bolt => {
            write_register(
                channel,
                reg::BOLT_DISCOVERY,
                [DISCOVERY_TIMEOUT, 0x02, 0x00],
            )
            .await
        }
        ReceiverFamily::Unifying => {
            write_register(channel, reg::UNIFYING_PAIRING, [0x02, 0x00, 0x00]).await
        }
    };
    if let Err(e) = res {
        debug!(?e, "cancel write failed");
    }
}

/// Removes the device on `slot` from the receiver named by `target`.
pub async fn unpair(target: ReceiverSelector, slot: u8) -> Result<(), PairingError> {
    let (channel, family) = open_receiver(&target).await?;
    match family {
        ReceiverFamily::Bolt => {
            let mut payload = [0u8; 16];
            payload[0] = 0x03; // action: unpair
            payload[1] = slot;
            write_long_register(&channel, reg::BOLT_PAIRING, payload).await
        }
        ReceiverFamily::Unifying => {
            write_register(&channel, reg::UNIFYING_PAIRING, [0x03, slot, 0x00]).await
        }
    }
}

async fn write_register(
    channel: &HidppChannel,
    address: u8,
    payload: [u8; 3],
) -> Result<(), PairingError> {
    channel
        .write_register(RECEIVER_INDEX, address, payload)
        .await
        .map_err(|e| {
            warn!(
                register = format_args!("{address:#04x}"),
                ?e,
                "register write failed"
            );
            PairingError::Register(format!("{e}"))
        })
}

async fn write_long_register(
    channel: &HidppChannel,
    address: u8,
    payload: [u8; 16],
) -> Result<(), PairingError> {
    channel
        .write_long_register(RECEIVER_INDEX, address, payload)
        .await
        .map_err(|e| {
            warn!(
                register = format_args!("{address:#04x}"),
                ?e,
                "long register write failed"
            );
            PairingError::Register(format!("{e}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a long HID++ message from a 17-byte payload (`p[0]` = address).
    fn long(sub_id: u8, device_index: u8, p: [u8; 17]) -> HidppMessage {
        let mut d = [0u8; 19];
        d[0] = device_index;
        d[1] = sub_id;
        d[2..19].copy_from_slice(&p);
        HidppMessage::Long(d)
    }

    #[test]
    fn decode_maps_long_payload_to_address_first() {
        let msg = long(notif::DEVICE_DISCOVERY, 0xff, {
            let mut p = [0u8; 17];
            p[0] = 0x07; // counter low (= Solaar address)
            p[1] = 0x00; // counter high (= Solaar data[0])
            p
        });
        let (idx, sub, payload) = decode(&msg);
        assert_eq!(idx, 0xff);
        assert_eq!(sub, notif::DEVICE_DISCOVERY);
        assert_eq!(payload[0], 0x07);
        assert_eq!(payload[1], 0x00);
    }

    #[test]
    fn parses_discovery_info_frame() {
        let mut p = [0u8; 17];
        p[0] = 0x05; // counter low
        p[1] = 0x00; // counter high
        p[2] = 0x00; // address frame selector
        p[4] = 0x02; // kind = mouse
        p[7..13].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef, 0x01, 0x02]);
        p[15] = 0x01; // auth: keyboard-typed bit
        assert_eq!(
            parse_notification(notif::DEVICE_DISCOVERY, 0xff, p),
            Some(Notification::DiscoveryInfo {
                counter: 5,
                kind: 0x02,
                address: [0xde, 0xad, 0xbe, 0xef, 0x01, 0x02],
                authentication: 0x01,
            })
        );
    }

    #[test]
    fn parses_discovery_name_frame() {
        let mut p = [0u8; 17];
        p[0] = 0x05;
        p[1] = 0x00;
        p[2] = 0x01; // name frame selector
        p[3] = 0x03; // length
        p[4..7].copy_from_slice(b"MX3");
        assert_eq!(
            parse_notification(notif::DEVICE_DISCOVERY, 0xff, p),
            Some(Notification::DiscoveryName {
                counter: 5,
                name: "MX3".to_string(),
            })
        );
    }

    #[test]
    fn parses_pairing_success_with_slot() {
        let mut p = [0u8; 17];
        p[0] = 0x02; // address 0x02 = complete
        p[1] = 0x00; // no error
        p[8] = 0x03; // slot = data[7]
        assert_eq!(
            parse_notification(notif::PAIRING_STATUS, 0xff, p),
            Some(Notification::PairingSucceeded { slot: 3 })
        );
    }

    #[test]
    fn parses_pairing_error() {
        let mut p = [0u8; 17];
        p[0] = 0x00;
        p[1] = 0x01; // BoltPairingError::DEVICE_TIMEOUT
        assert_eq!(
            parse_notification(notif::PAIRING_STATUS, 0xff, p),
            Some(Notification::PairingError(0x01))
        );
    }

    #[test]
    fn parses_passkey_digits() {
        let mut p = [0u8; 17];
        p[1..7].copy_from_slice(b"123456");
        assert_eq!(
            parse_notification(notif::PASSKEY_REQUEST, 0xff, p),
            Some(Notification::Passkey("123456".to_string()))
        );
    }

    #[test]
    fn parses_unifying_lock() {
        let mut p = [0u8; 17];
        p[0] = 0x01; // lock open
        assert_eq!(
            parse_notification(notif::UNIFYING_LOCK, 0xff, p),
            Some(Notification::UnifyingLock {
                open: true,
                error: 0
            })
        );
    }

    #[test]
    fn passkey_clicks_are_msb_first_10_bits() {
        // 0b00_0000_0101 = 5 -> eight lefts then right, left, right.
        assert_eq!(
            passkey_to_clicks("5"),
            vec![
                Click::Left,
                Click::Left,
                Click::Left,
                Click::Left,
                Click::Left,
                Click::Left,
                Click::Left,
                Click::Right,
                Click::Left,
                Click::Right,
            ]
        );
    }
}
