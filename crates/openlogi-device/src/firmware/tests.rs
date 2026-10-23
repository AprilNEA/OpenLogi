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
    Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Default, PartialEq, Eq)]
enum Fault {
    #[default]
    None,
    Staged,
    WrongModel,
    WrongUnit,
    EarlyComplete,
    DropReplyOnce,
    DropPackets {
        packet: usize,
        remaining: usize,
    },
    WaitForPoll,
    DropStartReply,
    StartNotAccepted,
    StartWrongEntity,
}

#[derive(Default)]
struct Hardware {
    applied: bool,
    packets: usize,
    packet_attempts: usize,
    start_count: usize,
    started: bool,
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
            payload[8] = u8::from(entity == if self.applied { target } else { active });
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
        self.started = true;
        self.target_entity = Some(entity);
        if matches!(self.fault, Fault::DropStartReply | Fault::StartWrongEntity) {
            return None;
        }
        let mut payload = [0; 16];
        payload[0] = entity;
        payload[1] = 2;
        Some(payload)
    }

    fn respond(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        if request.len() < 7 {
            return None;
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
                payload[0] = request[4];
                if self.packets == 4 {
                    payload[1] = 4;
                } else if self.packets > 0 {
                    payload[1] = 6;
                    payload[2..6]
                        .copy_from_slice(&u32::try_from(self.packets).unwrap().to_be_bytes());
                } else {
                    payload[1] = if self.started {
                        2
                    } else if self.fault == Fault::Staged {
                        5
                    } else {
                        1
                    };
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
                self.packets += 1;
                self.sent_bytes.extend_from_slice(&request[4..20]);
                payload[0] = self.target_entity.unwrap_or(1);
                if self.fault == Fault::DropReplyOnce {
                    self.fault = Fault::None;
                    return None;
                }
                if self.packets == 4 && self.fault == Fault::WaitForPoll {
                    payload[1] = 3;
                    payload[3] = 1;
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
        let mut response = vec![0; 20];
        response[0] = 0x11;
        response[1..4].copy_from_slice(&request[1..4]);
        // Test the real RDFU convention: status arrives as software-ID-zero
        // event 0 rather than a header-echoed response.
        if request[2] == 4 && request[3] >> 4 != 0 {
            response[3] = 0;
        }
        response[4..].copy_from_slice(&payload);
        Some(response)
    }
}

async fn hardware(state: Hardware) -> (SharedChannel, Arc<Mutex<Hardware>>) {
    let state = Arc::new(Mutex::new(state));
    let responder = Arc::clone(&state);
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        responder.lock().unwrap().respond(request)
    });
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
async fn preflight_refuses_low_battery_wrong_model_and_unknown_staged_image_without_starting() {
    for state in [
        Hardware {
            battery: Some(29),
            ..Hardware::default()
        },
        Hardware {
            fault: Fault::WrongModel,
            ..Hardware::default()
        },
        Hardware {
            fault: Fault::Staged,
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
    let responder = Arc::clone(&state);
    let (raw, handle) = ScriptedRawHidChannel::with_failing_writes(
        move |request| responder.lock().unwrap().respond(request),
        // Drop the init payload (packet 1) before the responder receives it.
        // Native failures avoid spending real timeout intervals on each retry.
        |request| request[2] == 4 && request[3] >> 4 == 4 && request.get(4..8) == Some(b"\x01RBM"),
    );
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
