use super::*;

#[tokio::test(start_paused = true)]
async fn cooperative_stop_and_deadline_prevent_the_next_command() {
    let started = tokio::time::Instant::now();
    assert!(matches!(
        check_stop(started, &mut || true),
        Err(FirmwareUpdateFailure::Protocol(_))
    ));
    check_stop(started, &mut || false).unwrap();
    tokio::time::advance(UPDATE_DEADLINE).await;
    assert!(matches!(
        check_stop(started, &mut || false),
        Err(FirmwareUpdateFailure::Protocol(_))
    ));
}

use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Default, PartialEq, Eq)]
enum Fault {
    #[default]
    None,
    WrongModel,
    WrongUnit,
    EarlyComplete,
    DropReplyOnce,
    DropPackets {
        packet: usize,
        remaining: usize,
    },
    /// Packet 4 asks to wait, and its status broadcast never arrives.
    WaitForPoll,
    /// Packet 4 asks to wait, then broadcasts completion like the recorded
    /// device does.
    WaitThenBroadcast,
    /// Packet 2 asks to wait; its broadcast misses the wait and lands while
    /// the next packet's command is outstanding. Packet 4 then waits and
    /// broadcasts completion.
    LateBroadcast,
    DropStartReply,
    StartNotAccepted,
    StartWrongEntity,
    /// Answers the HID++ 2.0 ping like a HID++ 1.0 device.
    Hidpp10,
    /// Reports no application entity as running.
    NoActiveApplication,
}

/// What an earlier, interrupted run left for the target bank, as the status
/// bytes after the entity.
#[derive(Clone, Copy)]
struct Leftover {
    status: [u8; 5],
    /// `startDfu` continues this state instead of restarting at block zero.
    start_resumes: bool,
}

const LEFTOVER_BLOCK_0: [u8; 5] = [0x02, 0, 0, 0, 0];
const LEFTOVER_MID_BLOCK_0: [u8; 5] = [0x06, 0, 0, 0, 2];
const LEFTOVER_MIDDLE_BLOCK: [u8; 5] = [0x02, 0, 7, 0, 0];
const LEFTOVER_COMPLETE: [u8; 5] = [0x04, 0, 0, 0, 0];
const LEFTOVER_APPLY_PENDING: [u8; 5] = [0x05, 0, 0, 0, 0];
const LEFTOVER_ABORTED: [u8; 5] = [0x07, 0, 0, 0, 0];

#[derive(Default)]
struct Hardware {
    applied: bool,
    packets: usize,
    packet_attempts: usize,
    start_count: usize,
    status_polls: usize,
    started: bool,
    leftover: Option<Leftover>,
    device_index: u8,
    /// Reports the device sends before / after its reply to the current
    /// request; the responder flushes them in order.
    before: Vec<Vec<u8>>,
    after: Vec<Vec<u8>>,
    battery: Option<u8>,
    running_version: Option<(u8, u8)>,
    active_entity: Option<u8>,
    target_entity: Option<u8>,
    fault: Fault,
    sent_bytes: Vec<u8>,
}

impl Hardware {
    fn firmware_info(&self, entity: u8) -> [u8; 16] {
        let mut payload = [0; 16];
        if entity == 0 {
            payload[0] = 1;
            payload[1..4].copy_from_slice(b"LD\0");
        } else {
            payload[1..4].copy_from_slice(b"RBM");
            payload[4] = 0x27;
            let active = self.active_entity.unwrap_or(2);
            let target = self.target_entity.unwrap_or(1);
            payload[5] = if self.applied && entity == target {
                3
            } else {
                1
            };
            payload[7] = if self.applied && entity == target {
                0x19
            } else {
                0x16
            };
            if !self.applied
                && entity == active
                && let Some((revision, build)) = self.running_version
            {
                payload[5] = revision;
                payload[7] = build;
            }
            payload[8] = u8::from(
                self.fault != Fault::NoActiveApplication
                    && entity == if self.applied { target } else { active },
            );
            payload[9..11].copy_from_slice(
                &(if self.fault == Fault::WrongModel {
                    0xb034_u16
                } else {
                    0xb042
                })
                .to_be_bytes(),
            );
        }
        payload
    }

    fn start_dfu(&mut self, entity: u8) -> Option<[u8; 16]> {
        self.start_count += 1;
        if self.fault == Fault::StartNotAccepted {
            return None;
        }
        let resumed = self.leftover.filter(|leftover| leftover.start_resumes);
        if resumed.is_none() {
            // A restart discards whatever an earlier run transferred.
            self.leftover = None;
            self.packets = 0;
            self.sent_bytes.clear();
        }
        self.started = true;
        self.target_entity = Some(entity);
        if matches!(self.fault, Fault::DropStartReply | Fault::StartWrongEntity) {
            return None;
        }
        let mut payload = [0; 16];
        payload[0] = entity;
        match resumed {
            Some(leftover) => payload[1..6].copy_from_slice(&leftover.status),
            None => payload[1] = 2,
        }
        Some(payload)
    }

    /// A status broadcast: software ID 0, event 0, on the DFU feature.
    fn broadcast(&self, status: &[u8]) -> Vec<u8> {
        let mut report = vec![0; 20];
        report[..3].copy_from_slice(&[0x11, self.device_index, 4]);
        report[4] = self.target_entity.unwrap_or(1);
        report[5..5 + status.len()].copy_from_slice(status);
        report
    }

    fn respond(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        if request.len() < 7 {
            return None;
        }
        self.device_index = request[1];
        if self.fault == Fault::Hidpp10 && (request[2], request[3] >> 4) == (0, 1) {
            // HID++ 1.0 error report: sub-id 0x8f, our feature index and
            // function/software byte echoed, then "invalid sub-id".
            return Some(vec![0x10, request[1], 0x8f, 0x00, request[3], 0x01, 0x00]);
        }
        let mut payload = [0; 16];
        match (request[2], request[3] >> 4) {
            (0, 1) => payload[0] = 4,
            (0, 0) => {
                payload[0] = match u16::from_be_bytes([request[4], request[5]]) {
                    3 => 3,
                    0xd1 => 4,
                    0x1004 => 5,
                    _ => 0,
                }
            }
            (3, 0) => {
                payload[0] = 3;
                payload[1..5].copy_from_slice(&[1, 2, 3, u8::from(self.fault == Fault::WrongUnit)]);
            }
            (3, 1) => payload = self.firmware_info(request[4]),
            (5, 0) => {
                payload[0] = 0x0f;
                payload[1] = 3;
            }
            (5, 1) => {
                payload[0] = self.battery.unwrap_or(75);
                payload[1] = if payload[0] < 30 { 2 } else { 4 };
            }
            (4, 0) => payload[0] = 7,
            (4, 2) => {
                self.status_polls += 1;
                payload[0] = request[4];
                if self.packets == 4 {
                    payload[1] = 4;
                } else if self.packets > 0 {
                    payload[1] = 6;
                    payload[2..6]
                        .copy_from_slice(&u32::try_from(self.packets).unwrap().to_be_bytes());
                } else if let Some(leftover) = self.leftover {
                    payload[1..6].copy_from_slice(&leftover.status);
                } else {
                    payload[1] = if self.started { 2 } else { 1 };
                }
                if self.started && self.fault == Fault::StartWrongEntity {
                    payload[0] = 3 - request[4];
                }
            }
            (4, 1) => payload = self.start_dfu(request[4])?,
            (4, 4) => {
                self.packet_attempts += 1;
                if let Fault::DropPackets { packet, remaining } = &mut self.fault
                    && self.packets == *packet
                    && *remaining > 0
                {
                    *remaining -= 1;
                    return None;
                }
                if self.fault == Fault::LateBroadcast && self.packets == 2 {
                    // The broadcast that should have ended packet 2's wait.
                    self.before.push(self.broadcast(&[6, 0, 0, 0, 2]));
                }
                self.packets += 1;
                self.sent_bytes.extend_from_slice(&request[4..20]);
                payload[0] = self.target_entity.unwrap_or(1);
                if self.fault == Fault::DropReplyOnce {
                    self.fault = Fault::None;
                    return None;
                }
                let waits = match self.fault {
                    Fault::WaitForPoll | Fault::WaitThenBroadcast => self.packets == 4,
                    Fault::LateBroadcast => matches!(self.packets, 2 | 4),
                    _ => false,
                };
                if waits {
                    payload[1] = 3;
                    payload[3] = 1;
                    if self.packets == 4 && self.fault != Fault::WaitForPoll {
                        self.after.push(self.broadcast(&[4]));
                    }
                } else if self.packets == 4 || self.fault == Fault::EarlyComplete {
                    payload[1] = 4;
                } else {
                    payload[1] = 6;
                    payload[2..6]
                        .copy_from_slice(&u32::try_from(self.packets).unwrap().to_be_bytes());
                }
            }
            (4, 3) => {
                self.applied = true;
                return None;
            }
            _ => return None,
        }
        // The recorded device echoes each request's header, DFU commands
        // included; status broadcasts only follow a wait (see `broadcast`).
        let mut response = vec![0; 20];
        response[0] = 0x11;
        response[1..4].copy_from_slice(&request[1..4]);
        response[4..].copy_from_slice(&payload);
        Some(response)
    }
}

/// Where the responder sends reports, set once the channel exists.
type Incoming = Arc<OnceLock<UnboundedSender<Vec<u8>>>>;

/// Answers each request from `state`, sending the reports the device emits
/// around its reply in wire order.
fn responder(
    state: &Arc<Mutex<Hardware>>,
) -> (
    impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
    Incoming,
) {
    let state = Arc::clone(state);
    let incoming: Incoming = Arc::new(OnceLock::new());
    let sender = Arc::clone(&incoming);
    let respond = move |request: &[u8]| {
        let mut state = state.lock().unwrap();
        let reply = state.respond(request);
        let before = std::mem::take(&mut state.before);
        let after = std::mem::take(&mut state.after);
        let incoming = sender.get().expect("responder is wired to its channel");
        for report in before.into_iter().chain(reply).chain(after) {
            let _ = incoming.send(report);
        }
        None
    };
    (respond, incoming)
}

async fn hardware(state: Hardware) -> (SharedChannel, Arc<Mutex<Hardware>>) {
    let state = Arc::new(Mutex::new(state));
    let (respond, incoming) = responder(&state);
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(respond);
    incoming.set(raw.incoming_sender()).unwrap();
    let shared = SharedChannel::new(
        scripted_channel(raw).await,
        DeviceRoute::Bolt {
            receiver_uid: "firmware-test".into(),
            slot: 2,
        },
    );
    (shared, state)
}

fn package() -> FirmwarePackage {
    FirmwarePackage::parse(&serde_json::to_vec(&package::tests::container()).unwrap()).unwrap()
}

#[tokio::test]
async fn accepted_start_with_lost_response_recovers_without_starting_again() {
    let (shared, state) = hardware(Hardware {
        fault: Fault::DropStartReply,
        ..Hardware::default()
    })
    .await;
    let target = update_target_on(&shared).await.unwrap().unwrap();
    let pending = prepare_update_on(&shared, target, &package())
        .await
        .unwrap()
        .install(|_| {}, || false)
        .await
        .unwrap();
    pending.verify_on(&shared).await.unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.start_count, 1);
    assert_eq!(state.sent_bytes, package().images[0].blocks[0]);
    assert!(state.applied);
}

#[tokio::test]
async fn uncertain_start_refuses_not_started_and_wrong_entity_without_sending_packets() {
    for fault in [Fault::StartNotAccepted, Fault::StartWrongEntity] {
        let (shared, state) = hardware(Hardware {
            fault,
            ..Hardware::default()
        })
        .await;
        let target = update_target_on(&shared).await.unwrap().unwrap();
        let result = prepare_update_on(&shared, target, &package())
            .await
            .unwrap()
            .install(|_| {}, || false)
            .await;
        assert!(matches!(result, Err(FirmwareUpdateFailure::Protocol(_))));
        let state = state.lock().unwrap();
        assert_eq!(state.start_count, 1);
        assert_eq!(state.packet_attempts, 0);
        assert!(!state.applied);
    }
}

#[tokio::test]
async fn reads_offer_transfers_exact_inactive_payload_and_verifies_activation() {
    let (shared, hardware) = hardware(Hardware::default()).await;
    let status = get_firmware_status_on(&shared).await.unwrap();
    assert_eq!(status.running.to_string(), "27.1.16");
    assert_eq!(
        status.offer,
        FirmwareOffer::Available(FirmwareVersion {
            number: 27,
            revision: 3,
            build: 19
        })
    );
    let target = update_target_on(&shared).await.unwrap().unwrap();
    let prepared = prepare_update_on(&shared, target, &package())
        .await
        .unwrap();
    let mut progress = Vec::new();
    let pending = prepared
        .install(|phase| progress.push(phase), || false)
        .await
        .unwrap();
    assert_eq!(hardware.lock().unwrap().packets, 4);
    assert_eq!(
        hardware.lock().unwrap().sent_bytes,
        package().images[0].blocks[0]
    );
    assert!(hardware.lock().unwrap().applied);
    assert_eq!(progress.last(), Some(&FirmwareUpdatePhase::Restarting));
    pending.verify_on(&shared).await.unwrap();
    assert_eq!(
        get_firmware_status_on(&shared).await.unwrap().offer,
        FirmwareOffer::UpToDate
    );
    hardware.lock().unwrap().fault = Fault::WrongUnit;
    assert_eq!(
        pending.verify_on(&shared).await,
        Err(FirmwareUpdateFailure::WrongDevice)
    );
}

#[tokio::test]
async fn preflight_refuses_low_battery_and_wrong_model_without_starting() {
    for state in [
        Hardware {
            battery: Some(29),
            ..Hardware::default()
        },
        Hardware {
            fault: Fault::WrongModel,
            ..Hardware::default()
        },
    ] {
        let (shared, hardware) = hardware(Hardware::default()).await;
        let target = update_target_on(&shared).await.unwrap().unwrap();
        *hardware.lock().unwrap() = state;
        assert!(
            prepare_update_on(&shared, target, &package())
                .await
                .is_err(),
            "preflight must refuse"
        );
        assert_eq!(hardware.lock().unwrap().start_count, 0);
        assert!(!hardware.lock().unwrap().applied);
    }
}

#[tokio::test]
async fn preflight_accepts_exactly_thirty_percent_charge() {
    let (shared, state) = hardware(Hardware {
        battery: Some(30),
        ..Hardware::default()
    })
    .await;
    let target = update_target_on(&shared).await.unwrap().unwrap();
    prepare_update_on(&shared, target, &package())
        .await
        .unwrap();
    assert_eq!(
        state.lock().unwrap().start_count,
        0,
        "preflight does not start DFU"
    );
}

#[tokio::test]
async fn early_completion_and_cooperative_stop_never_apply() {
    let (shared, state) = hardware(Hardware {
        fault: Fault::EarlyComplete,
        ..Hardware::default()
    })
    .await;
    let target = update_target_on(&shared).await.unwrap().unwrap();
    let prepared = prepare_update_on(&shared, target, &package())
        .await
        .unwrap();
    assert!(matches!(
        prepared.install(|_| {}, || false).await,
        Err(FirmwareUpdateFailure::Protocol(_))
    ));
    assert!(!state.lock().unwrap().applied);

    let (shared, state) = hardware(Hardware::default()).await;
    let target = update_target_on(&shared).await.unwrap().unwrap();
    let prepared = prepare_update_on(&shared, target, &package())
        .await
        .unwrap();
    let stopped = AtomicBool::new(false);
    let result = prepared
        .install(
            |phase| {
                if phase == (FirmwareUpdatePhase::Transferring { percent: 100 }) {
                    stopped.store(true, Ordering::SeqCst);
                }
            },
            || stopped.load(Ordering::SeqCst),
        )
        .await;
    assert!(matches!(result, Err(FirmwareUpdateFailure::Protocol(_))));
    assert_eq!(state.lock().unwrap().packets, 4);
    assert!(!state.lock().unwrap().applied);
}

#[tokio::test(start_paused = true)]
async fn missed_wait_broadcast_recovers_by_polling_before_apply() {
    let (shared, state) = hardware(Hardware {
        fault: Fault::WaitForPoll,
        ..Hardware::default()
    })
    .await;
    let target = update_target_on(&shared).await.unwrap().unwrap();
    prepare_update_on(&shared, target, &package())
        .await
        .unwrap()
        .install(|_| {}, || false)
        .await
        .unwrap();
    assert!(state.lock().unwrap().applied);
    assert_eq!(state.lock().unwrap().packets, 4);
}

#[tokio::test]
async fn lost_packet_reply_recovers_from_persisted_position_without_skipping_bytes() {
    let (shared, state) = hardware(Hardware {
        fault: Fault::DropReplyOnce,
        ..Hardware::default()
    })
    .await;
    let target = update_target_on(&shared).await.unwrap().unwrap();
    prepare_update_on(&shared, target, &package())
        .await
        .unwrap()
        .install(|_| {}, || false)
        .await
        .unwrap();
    assert!(state.lock().unwrap().applied);
    assert_eq!(
        state.lock().unwrap().sent_bytes,
        package().images[0].blocks[0]
    );
}

#[tokio::test]
async fn a_replacement_device_after_admission_never_starts_dfu() {
    let (original, _) = hardware(Hardware::default()).await;
    let target = update_target_on(&original).await.unwrap().unwrap();
    drop(original);
    let (shared, state) = hardware(Hardware {
        fault: Fault::WrongUnit,
        ..Hardware::default()
    })
    .await;
    let result = match prepare_update_on(&shared, target, &package()).await {
        Ok(prepared) => prepared.install(|_| {}, || false).await,
        Err(error) => Err(error),
    };
    assert_eq!(result.err(), Some(FirmwareUpdateFailure::WrongDevice));
    let state = state.lock().unwrap();
    assert_eq!(state.start_count, 0);
    assert_eq!(state.packet_attempts, 0);
    assert!(!state.applied);
}

#[tokio::test]
async fn the_admitted_device_can_reconnect_on_a_fresh_channel() {
    let (original, _) = hardware(Hardware::default()).await;
    let target = update_target_on(&original).await.unwrap().unwrap();
    drop(original);
    let (shared, state) = hardware(Hardware::default()).await;
    let pending = prepare_update_on(&shared, target, &package())
        .await
        .unwrap()
        .install(|_| {}, || false)
        .await
        .unwrap();
    pending.verify_on(&shared).await.unwrap();
    assert!(state.lock().unwrap().applied);
}

#[tokio::test(start_paused = true)]
async fn lost_packet_retries_the_requested_position_without_skipping_bytes() {
    for packet in [0, 1, 3] {
        let (shared, state) = hardware(Hardware {
            fault: Fault::DropPackets {
                packet,
                remaining: 1,
            },
            ..Hardware::default()
        })
        .await;
        let target = update_target_on(&shared).await.unwrap().unwrap();
        prepare_update_on(&shared, target, &package())
            .await
            .unwrap()
            .install(|_| {}, || false)
            .await
            .unwrap();
        let state = state.lock().unwrap();
        assert!(state.applied, "lost packet {packet} must be retried");
        assert_eq!(state.packet_attempts, 5);
        assert_eq!(state.sent_bytes, package().images[0].blocks[0]);
    }
}

#[tokio::test(start_paused = true)]
async fn repeated_packet_loss_exhausts_recovery_without_activating() {
    let (original, state) = hardware(Hardware::default()).await;
    let target = update_target_on(&original).await.unwrap().unwrap();
    let (respond, incoming) = responder(&state);
    let (raw, handle) = ScriptedRawHidChannel::with_failing_writes(
        respond,
        // Drop the init payload (packet 1) before the responder receives it.
        // Native failures avoid spending real timeout intervals on each retry.
        |request| request[2] == 4 && request[3] >> 4 == 4 && request.get(4..8) == Some(b"\x01RBM"),
    );
    incoming.set(raw.incoming_sender()).unwrap();
    let shared = SharedChannel::new(scripted_channel(raw).await, original.route().clone());
    drop(original);
    let result = prepare_update_on(&shared, target, &package())
        .await
        .unwrap()
        .install(|_| {}, || false)
        .await;
    assert!(matches!(result, Err(FirmwareUpdateFailure::Device(_))));
    let attempts = handle
        .written_reports()
        .iter()
        .filter(|request| request[2] == 4 && request[3] >> 4 == 4)
        .count();
    let state = state.lock().unwrap();
    assert_eq!(attempts, MAX_RECOVERIES + 2);
    assert_eq!(state.packets, 1);
    assert!(!state.applied);
}

#[tokio::test]
async fn current_release_can_be_reinstalled_into_the_inactive_bank() {
    for active in [1, 2] {
        let (shared, hardware) = hardware(Hardware {
            running_version: Some((3, 0x19)),
            active_entity: Some(active),
            ..Hardware::default()
        })
        .await;
        let status = get_firmware_status_on(&shared).await.unwrap();
        assert_eq!(status.running.to_string(), "27.3.19");
        assert_eq!(status.offer, FirmwareOffer::UpToDate);
        let target = update_target_on(&shared).await.unwrap().unwrap();
        let pending = prepare_update_on(&shared, target, &package())
            .await
            .unwrap()
            .install(|_| {}, || false)
            .await
            .unwrap();
        pending.verify_on(&shared).await.unwrap();
        let state = hardware.lock().unwrap();
        assert_eq!(state.target_entity, Some(3 - active));
        assert_eq!(state.start_count, 1);
        assert_eq!(state.packets, 4);
        assert!(
            state.applied,
            "reinstallation switches to the other application bank"
        );
    }
}

#[tokio::test]
async fn newer_than_catalog_is_not_installable_and_never_downgrades() {
    let (shared, hardware) = hardware(Hardware::default()).await;
    let target = update_target_on(&shared).await.unwrap().unwrap();
    hardware.lock().unwrap().running_version = Some((3, 0x20));
    let status = get_firmware_status_on(&shared).await.unwrap();
    assert_eq!(status.running.to_string(), "27.3.20");
    assert_eq!(status.offer, FirmwareOffer::Unavailable);
    assert_eq!(update_target_on(&shared).await.unwrap(), None);
    assert!(matches!(
        prepare_update_on(&shared, target, &package()).await,
        Err(FirmwareUpdateFailure::WrongDevice)
    ));
    let state = hardware.lock().unwrap();
    assert_eq!(state.start_count, 0);
    assert!(!state.applied);
}

#[tokio::test]
async fn devices_without_a_readable_firmware_table_are_unsupported_not_unreachable() {
    for fault in [Fault::Hidpp10, Fault::NoActiveApplication] {
        let (shared, _) = hardware(Hardware {
            fault,
            ..Hardware::default()
        })
        .await;
        assert!(matches!(
            get_firmware_status_on(&shared).await,
            Err(WriteError::FeatureUnsupported {
                feature_hex: 0x0003
            })
        ));
    }
}

fn leftover(status: [u8; 5], start_resumes: bool) -> Hardware {
    Hardware {
        leftover: Some(Leftover {
            status,
            start_resumes,
        }),
        ..Hardware::default()
    }
}

async fn update(
    shared: &SharedChannel,
) -> Result<PendingFirmwareActivation, FirmwareUpdateFailure> {
    let target = update_target_on(shared).await.unwrap().unwrap();
    prepare_update_on(shared, target, &package())
        .await?
        .install(|_| {}, || false)
        .await
}

#[tokio::test]
async fn an_interrupted_transfer_restarts_from_the_first_block_and_sends_every_packet() {
    for state in [
        leftover(LEFTOVER_BLOCK_0, false),
        // Indistinguishable from a restart, and every packet is still sent.
        leftover(LEFTOVER_BLOCK_0, true),
        leftover(LEFTOVER_MID_BLOCK_0, false),
        leftover(LEFTOVER_MIDDLE_BLOCK, false),
        leftover(LEFTOVER_COMPLETE, false),
        leftover(LEFTOVER_APPLY_PENDING, false),
        leftover(LEFTOVER_ABORTED, false),
    ] {
        let status = state.leftover.unwrap().status;
        let (shared, hardware) = hardware(state).await;
        let target = update_target_on(&shared).await.unwrap().unwrap();
        let prepared = prepare_update_on(&shared, target, &package())
            .await
            .unwrap_or_else(|error| panic!("{status:?} must not block a retry: {error:?}"));
        assert_eq!(hardware.lock().unwrap().start_count, 0);
        let pending = prepared.install(|_| {}, || false).await.unwrap();
        pending.verify_on(&shared).await.unwrap();
        let state = hardware.lock().unwrap();
        assert_eq!(state.start_count, 1, "{status:?}");
        assert_eq!(state.packets, 4, "{status:?}");
        assert_eq!(
            state.sent_bytes,
            package().images[0].blocks[0],
            "{status:?}"
        );
        assert!(state.applied, "{status:?}");
    }
}

#[tokio::test]
async fn a_device_that_resumes_an_earlier_transfer_is_never_applied() {
    for status in [
        LEFTOVER_MID_BLOCK_0,
        LEFTOVER_MIDDLE_BLOCK,
        LEFTOVER_COMPLETE,
        LEFTOVER_APPLY_PENDING,
    ] {
        let (shared, hardware) = hardware(leftover(status, true)).await;
        let result = update(&shared).await;
        assert!(
            matches!(&result, Err(FirmwareUpdateFailure::Protocol(message)) if message.contains("earlier transfer")),
            "{status:?}: {result:?}"
        );
        let state = hardware.lock().unwrap();
        assert_eq!(state.start_count, 1);
        assert_eq!(state.packet_attempts, 0, "{status:?}");
        assert!(!state.applied, "{status:?}");
    }
}

#[tokio::test]
async fn an_unconfirmed_restart_fails_closed_and_the_next_attempt_restarts() {
    let mut state = leftover(LEFTOVER_MIDDLE_BLOCK, false);
    state.fault = Fault::DropStartReply;
    let (shared, hardware) = hardware(state).await;
    assert!(matches!(
        update(&shared).await,
        Err(FirmwareUpdateFailure::Protocol(_))
    ));
    {
        let state = hardware.lock().unwrap();
        assert_eq!(
            state.start_count, 1,
            "a lost restart is never re-sent blindly"
        );
        assert_eq!(state.packet_attempts, 0);
        assert!(!state.applied);
    }
    hardware.lock().unwrap().fault = Fault::None;
    update(&shared)
        .await
        .unwrap()
        .verify_on(&shared)
        .await
        .unwrap();
    let state = hardware.lock().unwrap();
    assert_eq!(state.start_count, 2);
    assert_eq!(state.sent_bytes, package().images[0].blocks[0]);
    assert!(state.applied);
}

/// Stops a run once it publishes `percent`, then retries on the same device.
#[tokio::test]
async fn a_retry_after_an_interrupted_run_transfers_everything_again() {
    for percent in [0, 50, 100] {
        let (shared, hardware) = hardware(Hardware::default()).await;
        let target = update_target_on(&shared).await.unwrap().unwrap();
        let stopped = AtomicBool::new(false);
        let interrupted = prepare_update_on(&shared, target, &package())
            .await
            .unwrap()
            .install(
                |phase| {
                    if phase == (FirmwareUpdatePhase::Transferring { percent }) {
                        stopped.store(true, Ordering::SeqCst);
                    }
                },
                || stopped.load(Ordering::SeqCst),
            )
            .await;
        interrupted.unwrap_err();
        assert!(!hardware.lock().unwrap().applied);

        update(&shared)
            .await
            .unwrap()
            .verify_on(&shared)
            .await
            .unwrap();
        let state = hardware.lock().unwrap();
        assert_eq!(state.start_count, 2, "after {percent}%");
        assert_eq!(
            state.sent_bytes,
            package().images[0].blocks[0],
            "after {percent}%"
        );
        assert!(state.applied, "after {percent}%");
    }
}

#[tokio::test]
async fn a_wait_ends_on_the_broadcast_sent_after_its_reply() {
    let (shared, hardware) = hardware(Hardware {
        fault: Fault::WaitThenBroadcast,
        ..Hardware::default()
    })
    .await;
    update(&shared).await.unwrap();
    let state = hardware.lock().unwrap();
    assert!(state.applied);
    assert_eq!(
        state.status_polls, 1,
        "only preflight polls; the broadcast ends the wait"
    );
}

#[tokio::test]
async fn a_broadcast_that_missed_its_wait_is_not_a_later_answer() {
    let (shared, hardware) = hardware(Hardware {
        fault: Fault::LateBroadcast,
        ..Hardware::default()
    })
    .await;
    update(&shared).await.unwrap();
    let state = hardware.lock().unwrap();
    assert!(state.applied);
    assert_eq!(state.packet_attempts, 4);
    assert_eq!(state.sent_bytes, package().images[0].blocks[0]);
}
