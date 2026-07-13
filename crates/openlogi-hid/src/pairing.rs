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
use tracing::{debug, trace};

pub use hidpp::receiver::bolt::DeviceKind as BoltDeviceKind;

use crate::transport::{enumerate_hidpp_devices, open_hidpp_channel};

mod notification;
mod registers;

use notification::{Notification, decode, parse_notification, subscribe};
use registers::{
    BOLT_DISCOVERY, BOLT_PAIRING, NOTIFICATION_FLAGS, NOTIFICATIONS, UNIFYING_PAIRING,
    write_long_register, write_register,
};

/// HID++ device index addressing the receiver itself (not a paired device).
const RECEIVER_INDEX: u8 = 0xff;

/// Receiver pairing family. Each uses a different register flow.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReceiverFamily {
    /// Logi Bolt receiver.
    Bolt,
    /// Logitech Unifying receiver.
    Unifying,
}

fn family_for(product_id: u16) -> Option<ReceiverFamily> {
    if crate::BOLT_PIDS.contains(&product_id) {
        Some(ReceiverFamily::Bolt)
    } else if crate::speaks_unifying_protocol(product_id) {
        // Unifying proper plus protocol-compatible Lightspeed receivers.
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
    /// Receiver protocol family.
    pub family: ReceiverFamily,
    /// USB product ID of the receiver.
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
    /// Device class reported by the receiver discovery notification.
    pub kind: BoltDeviceKind,
    /// Human-readable name advertised by the discovered device.
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
    /// Left mouse button click.
    Left,
    /// Right mouse button click.
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
    Pointer {
        /// Numeric passkey shown by the device.
        passkey: String,
        /// MSB-first click sequence derived from the passkey.
        clicks: Vec<Click>,
    },
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
    /// A device was paired and assigned a receiver slot.
    Paired {
        /// Assigned pairing slot.
        slot: u8,
    },
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
    /// HID transport failure.
    #[error("HID transport error: {0}")]
    Hid(String),
    /// No supported receiver matched the requested selector.
    #[error("no supported pairing-capable receiver found")]
    ReceiverNotFound,
    /// HID++ receiver register read/write failed.
    #[error("receiver register access failed: {0}")]
    Register(String),
    /// Pairing flow exceeded its timeout.
    #[error("pairing timed out")]
    Timeout,
    /// Receiver reported a device-specific pairing error code.
    #[error("receiver reported pairing error {0:#04x}")]
    Device(u8),
    /// Pairing flow was cancelled by the caller.
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
    let (channel, family) = match open_receiver(&target).await {
        Ok(receiver) => receiver,
        Err(e) => {
            let _ = events.send(PairingEvent::Failed(e.clone()));
            return Err(e);
        }
    };
    let (listener, mut notifications) = subscribe(&channel);

    let result = drive(&channel, family, &mut commands, &mut notifications, &events).await;

    drop(listener);
    // Best-effort restore: clear notification flags we set.
    let _ = channel
        .write_register(RECEIVER_INDEX, NOTIFICATIONS, [0, 0, 0])
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
    write_register(channel, NOTIFICATIONS, NOTIFICATION_FLAGS).await?;

    match family {
        ReceiverFamily::Bolt => {
            write_register(channel, BOLT_DISCOVERY, [DISCOVERY_TIMEOUT, 0x01, 0x00]).await?;
        }
        ReceiverFamily::Unifying => {
            write_register(channel, UNIFYING_PAIRING, [0x01, 0x00, DISCOVERY_TIMEOUT]).await?;
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
    write_long_register(channel, BOLT_PAIRING, payload).await
}

/// Best-effort cancel of an in-progress flow.
async fn cancel(channel: &HidppChannel, family: ReceiverFamily) {
    let res = match family {
        ReceiverFamily::Bolt => {
            write_register(channel, BOLT_DISCOVERY, [DISCOVERY_TIMEOUT, 0x02, 0x00]).await
        }
        ReceiverFamily::Unifying => {
            write_register(channel, UNIFYING_PAIRING, [0x02, 0x00, 0x00]).await
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
            write_long_register(&channel, BOLT_PAIRING, payload).await
        }
        ReceiverFamily::Unifying => {
            write_register(&channel, UNIFYING_PAIRING, [0x03, slot, 0x00]).await
        }
    }
}

#[cfg(test)]
mod tests;
