use hidpp::channel::HidppChannel;
use hidpp::protocol::v10::{ErrorType, Hidpp10Error};
use hidpp::receiver::bolt;

use super::*;

const ONLINE: ReceiverSlotState = ReceiverSlotState::Paired(ReceiverLinkState::Online);
const OFFLINE: ReceiverSlotState = ReceiverSlotState::Paired(ReceiverLinkState::Offline);

#[tokio::test]
async fn dpi_rejects_unavailable_slot_without_consuming_success_and_recovers() {
    for unavailable in [OFFLINE, ReceiverSlotState::Empty] {
        let mut fixture = receiver_dpi_fixture();
        let original = fixture.cassette.exchanges.clone();
        let mut recovery = original.clone();
        recovery[3].response = Some(short(1, 5, 0x20, [0, 0x04, 0xb0]));
        fixture.cassette.exchanges.extend(recovery);
        fixture.cassette.exchanges.push(original[0].clone()); // receiver UID while slot 1 is unavailable
        let mut other = original;
        for exchange in &mut other[1..] {
            exchange.request[1] = 2;
            exchange.response.as_mut().unwrap()[1] = 2;
        }
        other[3].response = Some(short(2, 5, 0x20, [0, 0x06, 0x40]));
        fixture.cassette.exchanges.extend(other);
        fixture.topology.nodes[0].receiver_slots.push(ReceiverSlot {
            slot: 2,
            state: ONLINE,
        });
        let backend = ReplayBackend::new(fixture.topology, vec![fixture.cassette]).unwrap();

        assert_eq!(
            get_dpi(&backend, &fixture.route).await.unwrap(),
            Dpi::new(800)
        );
        backend
            .set_receiver_slot_state(&fixture.node_id, 1, unavailable)
            .unwrap();
        let held_ping = backend
            .hold_next_response(
                RECEIVER_CHANNEL,
                RequestMatch::Hidpp20,
                &short(1, 0, 0x10, [0; 3]),
            )
            .unwrap();
        get_dpi(&backend, &fixture.route)
            .await
            .expect_err("unavailable slot cannot answer the online cassette");
        assert!(
            !held_ping.is_request_written(),
            "rejection must not consume the response barrier"
        );
        let failed = backend.channel_completion(RECEIVER_CHANNEL).unwrap();
        assert_eq!(
            failed.unconsumed_required.len(),
            8,
            "only receiver UID is consumed by the failed operation"
        );
        assert_eq!(failed.slot_state_errors.len(), 1);
        assert!(failed.unmatched_requests.is_empty());
        let diagnostic = backend.require_complete().unwrap_err().to_string();
        assert!(diagnostic.contains("slot 1"), "{diagnostic}");
        assert!(
            diagnostic.contains(&format!("{unavailable:?}")),
            "{diagnostic}"
        );

        let other_route = DeviceRoute::Bolt {
            receiver_uid: RECEIVER_UID.to_string(),
            slot: 2,
        };
        assert_eq!(
            get_dpi(&backend, &other_route).await.unwrap(),
            Dpi::new(1600)
        );
        assert_eq!(
            backend.enumerate().await.unwrap().len(),
            1,
            "receiver remains reachable"
        );
        backend
            .set_receiver_slot_state(&fixture.node_id, 1, ONLINE)
            .unwrap();
        let (recovered, ()) = tokio::join!(get_dpi(&backend, &fixture.route), async {
            held_ping.request_written().await;
            held_ping.release();
        });
        assert_eq!(
            recovered.unwrap(),
            Dpi::new(1200),
            "the queued recovery cassette was retained"
        );
        let completed = backend.channel_completion(RECEIVER_CHANNEL).unwrap();
        assert!(completed.unconsumed_required.is_empty());
        assert!(completed.unmatched_requests.is_empty());
        assert_eq!(
            completed.slot_state_errors, failed.slot_state_errors,
            "diagnostics are sticky after recovery"
        );
        assert!(!completed.is_complete());
        assert_eq!(
            backend.require_complete().unwrap_err().to_string(),
            diagnostic
        );
    }
}

#[tokio::test]
async fn slot_state_applies_to_existing_channels_and_optional_write_successes() {
    let mut fixture = receiver_dpi_fixture();
    let request = short(1, 0x80, 0x10, [1, 2, 3]);
    fixture.cassette.exchanges = vec![CassetteExchange {
        request_match: RequestMatch::Exact,
        request: request.clone(),
        response: Some(request),
        required: false,
    }];
    let node = fixture.topology.nodes[0].info.clone();
    let backend = ReplayBackend::new(fixture.topology, vec![fixture.cassette]).unwrap();
    let channel = backend.open_hidpp(&node).await.unwrap().unwrap();
    backend
        .set_receiver_slot_state(&fixture.node_id, 1, OFFLINE)
        .unwrap();
    channel
        .write_register(1, 0x10, [1, 2, 3])
        .await
        .expect_err("existing channel observes changed liveness");
    assert!(
        channel.is_connected(),
        "slot failure is not receiver disconnection"
    );
    let failed = backend.channel_completion(RECEIVER_CHANNEL).unwrap();
    assert_eq!((failed.consumed_optional, failed.unused_optional), (0, 1));
    assert!(
        !failed.is_complete(),
        "optional responses cannot hide slot contradictions"
    );
    backend
        .set_receiver_slot_state(&fixture.node_id, 1, ONLINE)
        .unwrap();
    channel.write_register(1, 0x10, [1, 2, 3]).await.unwrap();
    let completed = backend.channel_completion(RECEIVER_CHANNEL).unwrap();
    assert_eq!(
        (completed.consumed_optional, completed.unused_optional),
        (1, 0)
    );
}

#[tokio::test(start_paused = true)]
async fn recorded_slot_errors_and_silence_remain_consumable_without_emulation() {
    for (state, error) in [
        (OFFLINE, ErrorType::ResourceError),
        (ReceiverSlotState::Empty, ErrorType::UnknownDevice),
    ] {
        let mut fixture = receiver_dpi_fixture();
        fixture.topology.nodes[0].receiver_slots[0].state = state;
        let request = short(1, 0x81, 0, [0; 3]);
        fixture.cassette.exchanges = vec![
            CassetteExchange {
                request_match: RequestMatch::Exact,
                request: request.clone(),
                response: Some(vec![0x10, 1, 0x8f, 0x81, 0, error.into(), 0]),
                required: true,
            },
            CassetteExchange {
                request_match: RequestMatch::Exact,
                request,
                response: None,
                required: true,
            },
        ];
        let node = fixture.topology.nodes[0].info.clone();
        let backend = ReplayBackend::new(fixture.topology, vec![fixture.cassette]).unwrap();
        let channel = backend.open_hidpp(&node).await.unwrap().unwrap();
        assert!(
            matches!(channel.read_register(1, 0, [0; 3]).await, Err(Hidpp10Error::RegisterAccess(actual)) if actual == error)
        );
        channel
            .read_register(1, 0, [0; 3])
            .await
            .expect_err("recorded silence times out, never invents success");
        backend.require_complete().unwrap();
    }
}

#[tokio::test]
async fn receiver_pairing_evidence_checks_presence_and_reported_liveness() {
    for (state, status, valid) in [
        (OFFLINE, 0x42, true),
        (OFFLINE, 0x02, false),
        (ONLINE, 0x42, false),
        (ReceiverSlotState::Empty, 0x42, false),
        (ReceiverSlotState::Empty, 0x02, false),
    ] {
        let (backend, receiver, node) = pairing_backend(state, status).await;
        let result = receiver.get_device_pairing_information(1).await;
        if valid {
            let info = result.unwrap();
            assert!(!info.online);
            assert_eq!(info.wpid, 0xb35b);
            backend.require_complete().unwrap();
        } else {
            result.expect_err("receiver pairing information contradicts explicit state");
            assert_eq!(
                backend
                    .channel_completion(RECEIVER_CHANNEL)
                    .unwrap()
                    .unconsumed_required
                    .len(),
                1
            );
            let reported = if status & 0x40 == 0 { ONLINE } else { OFFLINE };
            backend.set_receiver_slot_state(&node, 1, reported).unwrap();
            assert_eq!(
                receiver
                    .get_device_pairing_information(1)
                    .await
                    .unwrap()
                    .online,
                reported == ONLINE
            );
            assert!(
                backend
                    .channel_completion(RECEIVER_CHANNEL)
                    .unwrap()
                    .unconsumed_required
                    .is_empty()
            );
            backend
                .require_complete()
                .expect_err("violation remains recorded after metadata recovery");
        }
    }
}

async fn pairing_backend(
    state: ReceiverSlotState,
    status: u8,
) -> (ReplayBackend, bolt::Receiver, NodeId) {
    let mut fixture = receiver_dpi_fixture();
    fixture.topology.nodes[0].receiver_slots[0].state = state;
    fixture.cassette.exchanges = vec![register_reply(0x51, &[0x51, status, 0x5b, 0xb3])];
    let node = fixture.topology.nodes[0].info.clone();
    let backend = ReplayBackend::new(fixture.topology, vec![fixture.cassette]).unwrap();
    let receiver = bolt::Receiver::new(backend.open_hidpp(&node).await.unwrap().unwrap()).unwrap();
    (backend, receiver, node.id)
}

#[tokio::test]
async fn receiver_names_remain_readable_offline_but_not_when_empty() {
    for sub in [0x61, 0x40] {
        let mut fixture = receiver_dpi_fixture();
        fixture.topology.nodes[0].receiver_slots[0].state = ReceiverSlotState::Empty;
        let payload = if sub == 0x61 {
            vec![sub, 1, 3, b'M', b'X', b'3']
        } else {
            vec![sub, 3, b'M', b'X', b'3']
        };
        let exchange = register_reply(sub, &payload);
        let parameters: [u8; 3] = exchange.request[4..].try_into().unwrap();
        fixture.cassette.exchanges = vec![exchange];
        let node = fixture.topology.nodes[0].info.clone();
        let backend = ReplayBackend::new(fixture.topology, vec![fixture.cassette]).unwrap();
        let channel = backend.open_hidpp(&node).await.unwrap().unwrap();
        channel
            .read_long_register(0xff, 0xb5, parameters)
            .await
            .expect_err("empty slot cannot expose a paired codename");
        backend
            .set_receiver_slot_state(&fixture.node_id, 1, OFFLINE)
            .unwrap();
        let response = channel
            .read_long_register(0xff, 0xb5, parameters)
            .await
            .unwrap();
        assert_eq!(&response[..payload.len()], payload);
        assert!(
            backend
                .channel_completion(RECEIVER_CHANNEL)
                .unwrap()
                .unconsumed_required
                .is_empty()
        );
    }
}

fn register_reply(sub: u8, payload: &[u8]) -> CassetteExchange {
    let mut response = vec![0; 20];
    response[..4].copy_from_slice(&[0x11, 0xff, 0x83, 0xb5]);
    response[4..4 + payload.len()].copy_from_slice(payload);
    CassetteExchange {
        request_match: RequestMatch::Exact,
        request: short(0xff, 0x83, 0xb5, [sub, u8::from(sub == 0x61), 0]),
        response: Some(response),
        required: true,
    }
}

#[tokio::test]
async fn arrival_notifications_require_matching_state_and_do_not_change_it() {
    let (backend, receiver, node) = pairing_backend(OFFLINE, 0x42).await;
    receiver.get_device_pairing_information(1).await.unwrap();
    let events = receiver.listen();
    let online = short(1, 0x41, 0, [0x02, 0x5b, 0xb3]);
    let offline = short(1, 0x41, 0, [0x42, 0x5b, 0xb3]);
    backend
        .emit_channel_report(RECEIVER_CHANNEL, &online)
        .expect_err("arrival cannot revive an explicitly offline slot");
    assert_eq!(
        backend
            .emit_channel_report(RECEIVER_CHANNEL, &offline)
            .unwrap(),
        1
    );
    assert!(
        matches!(events.recv().await.unwrap(), bolt::Event::DeviceConnection(connection) if !connection.online)
    );
    backend
        .set_receiver_slot_state(&node, 1, ReceiverSlotState::Empty)
        .unwrap();
    backend
        .emit_channel_report(RECEIVER_CHANNEL, &offline)
        .expect_err("offline arrival still claims a pairing");
    backend.set_receiver_slot_state(&node, 1, ONLINE).unwrap();
    backend
        .emit_channel_report(RECEIVER_CHANNEL, &online)
        .unwrap();
    assert!(
        matches!(events.recv().await.unwrap(), bolt::Event::DeviceConnection(connection) if connection.online)
    );
    assert!(
        events.try_recv().is_err(),
        "rejected reports were never delivered"
    );
    assert_eq!(
        backend
            .channel_completion(RECEIVER_CHANNEL)
            .unwrap()
            .slot_state_errors
            .len(),
        2
    );
    backend
        .require_complete()
        .expect_err("unsolicited contradictions also invalidate completion");
}

#[tokio::test]
async fn in_flight_response_stays_with_retired_lifetime_across_slot_recovery() {
    let mut fixture = receiver_dpi_fixture();
    let mut recovery = fixture.cassette.exchanges.clone();
    recovery[3].response = Some(short(1, 5, 0x20, [0, 0x06, 0x40]));
    fixture.cassette.exchanges.extend(recovery);
    let backend = Arc::new(ReplayBackend::new(fixture.topology, vec![fixture.cassette]).unwrap());
    let request = short(1, 5, 0x20, [0; 3]);
    let old = backend
        .hold_next_response(RECEIVER_CHANNEL, RequestMatch::Hidpp20, &request)
        .unwrap();
    let operation = spawn_dpi(&backend, &fixture.route);
    old.request_written().await;
    assert_eq!(
        backend
            .channel_completion(RECEIVER_CHANNEL)
            .unwrap()
            .unconsumed_required
            .len(),
        4
    );
    backend
        .set_receiver_slot_state(&fixture.node_id, 1, OFFLINE)
        .unwrap();
    backend
        .set_channel_connection(RECEIVER_CHANNEL, ChannelConnection::Disconnected)
        .unwrap();
    operation.abort();
    assert!(operation.await.unwrap_err().is_cancelled());
    assert_eq!(backend.channel_lifetime_count(RECEIVER_CHANNEL).unwrap(), 0);
    backend
        .set_receiver_slot_state(&fixture.node_id, 1, ONLINE)
        .unwrap();
    backend
        .set_channel_connection(RECEIVER_CHANNEL, ChannelConnection::Connected)
        .unwrap();
    let new = backend
        .hold_next_response(RECEIVER_CHANNEL, RequestMatch::Hidpp20, &request)
        .unwrap();
    let replacement = spawn_dpi(&backend, &fixture.route);
    new.request_written().await;
    old.release();
    tokio::task::yield_now().await;
    assert!(
        !replacement.is_finished(),
        "retired response cannot satisfy replacement request"
    );
    new.release();
    assert_eq!(replacement.await.unwrap().unwrap(), Dpi::new(1600));
    backend.require_complete().unwrap();
}

fn spawn_dpi(
    backend: &Arc<ReplayBackend>,
    route: &DeviceRoute,
) -> tokio::task::JoinHandle<Result<Dpi, crate::WriteError>> {
    let backend = Arc::clone(backend);
    let route = route.clone();
    tokio::spawn(async move { get_dpi(&*backend, &route).await })
}

#[tokio::test]
async fn cassette_arrival_cannot_bypass_explicit_slot_state() {
    let mut fixture = receiver_dpi_fixture();
    fixture.topology.nodes[0].receiver_slots[0].state = OFFLINE;
    fixture.cassette.exchanges = vec![CassetteExchange {
        request_match: RequestMatch::Exact,
        request: short(0xff, 0x80, 2, [2, 0, 0]),
        response: Some(short(1, 0x41, 0, [0x02, 0x5b, 0xb3])),
        required: true,
    }];
    let node = fixture.topology.nodes[0].info.clone();
    let backend = ReplayBackend::new(fixture.topology, vec![fixture.cassette]).unwrap();
    let receiver = bolt::Receiver::new(backend.open_hidpp(&node).await.unwrap().unwrap()).unwrap();
    let events = receiver.listen();
    receiver
        .trigger_device_arrival()
        .await
        .expect_err("cassette arrival claims a live offline slot");
    let completion = backend.channel_completion(RECEIVER_CHANNEL).unwrap();
    assert_eq!(completion.slot_state_errors.len(), 1);
    assert_eq!(completion.unconsumed_required.len(), 1);
    assert!(
        events.try_recv().is_err(),
        "contradictory arrival never reaches production listener"
    );
}

#[tokio::test]
async fn shared_channel_aliases_observe_one_slot_state_owner() {
    let mut fixture = receiver_dpi_fixture();
    let mut alias = fixture.topology.nodes[0].clone();
    alias.info.id = NodeId::from("receiver-alias".to_string());
    let alias_info = alias.info.clone();
    fixture.topology.nodes.push(alias);
    let backend =
        ReplayBackend::new(fixture.topology.clone(), vec![fixture.cassette.clone()]).unwrap();
    let channel: Arc<HidppChannel> = backend.open_hidpp(&alias_info).await.unwrap().unwrap();
    backend
        .set_receiver_slot_state(&fixture.node_id, 1, OFFLINE)
        .unwrap();
    // Use a production operation on the already-open alias; no getter-only proof.
    let device = hidpp::device::Device::new(channel, 1).await;
    assert!(
        device.is_err(),
        "alias must not retain its initial online declaration"
    );
    assert_eq!(
        backend
            .channel_completion(RECEIVER_CHANNEL)
            .unwrap()
            .slot_state_errors
            .len(),
        1
    );

    fixture.topology.nodes[1].receiver_slots[0].state = OFFLINE;
    let error = ReplayBackend::new(fixture.topology, vec![fixture.cassette])
        .err()
        .expect("contradictory aliases are invalid topology");
    assert!(
        error
            .to_string()
            .contains("shared channel disagrees on receiver slot 1")
    );
}

#[tokio::test]
async fn pairing_count_respects_declared_empty_and_offline_slots() {
    for (count, valid) in [(0, false), (1, true), (5, true), (6, false)] {
        let mut fixture = receiver_dpi_fixture();
        fixture.topology.nodes[0].receiver_slots = vec![
            ReceiverSlot {
                slot: 1,
                state: OFFLINE,
            },
            ReceiverSlot {
                slot: 6,
                state: ReceiverSlotState::Empty,
            },
        ];
        fixture.cassette.exchanges = vec![CassetteExchange {
            request_match: RequestMatch::Exact,
            request: short(0xff, 0x81, 2, [0; 3]),
            response: Some(short(0xff, 0x81, 2, [0, count, 0])),
            required: true,
        }];
        let node = fixture.topology.nodes[0].info.clone();
        let backend = ReplayBackend::new(fixture.topology, vec![fixture.cassette]).unwrap();
        let receiver =
            bolt::Receiver::new(backend.open_hidpp(&node).await.unwrap().unwrap()).unwrap();
        if valid {
            assert_eq!(receiver.count_pairings().await.unwrap(), count);
            backend.require_complete().unwrap();
        } else {
            receiver
                .count_pairings()
                .await
                .expect_err("pairing count contradicts known slots");
            let completion = backend.channel_completion(RECEIVER_CHANNEL).unwrap();
            assert_eq!(completion.unconsumed_required.len(), 1);
            assert_eq!(completion.slot_state_errors.len(), 1);
        }
    }
}

#[tokio::test]
async fn hidpp20_high_feature_success_is_not_mistaken_for_a_receiver_error() {
    use hidpp::channel::HidppMessage;
    use hidpp::protocol::v20::{self, Hidpp20Error};

    let mut fixture = receiver_dpi_fixture();
    fixture.topology.nodes[0].receiver_slots[0].state = OFFLINE;
    let request = short(1, 0x8f, 0x21, [0; 3]);
    fixture.cassette.exchanges = vec![
        h20(request.clone(), vec![0x10, 1, 0xff, 0x8f, 0x21, 2, 0]),
        h20(request.clone(), short(1, 0x8f, 0x21, [0, 0x03, 0x20])),
    ];
    let node = fixture.topology.nodes[0].info.clone();
    let backend = ReplayBackend::new(fixture.topology, vec![fixture.cassette]).unwrap();
    let channel = backend.open_hidpp(&node).await.unwrap().unwrap();
    let message = v20::Message::from(HidppMessage::read_raw(&request).unwrap());
    assert!(matches!(
        channel.send_v20(message).await,
        Err(Hidpp20Error::Feature(v20::ErrorType::InvalidArgument))
    ));
    channel
        .send_v20(message)
        .await
        .expect_err("0x8f success still requires a live device link");
    assert_eq!(
        backend
            .channel_completion(RECEIVER_CHANNEL)
            .unwrap()
            .unconsumed_required
            .len(),
        1
    );
    backend
        .set_receiver_slot_state(&fixture.node_id, 1, ONLINE)
        .unwrap();
    let response = channel.send_v20(message).await.unwrap();
    assert_eq!(&response.extend_payload()[..3], &[0, 0x03, 0x20]);
    assert!(
        backend
            .channel_completion(RECEIVER_CHANNEL)
            .unwrap()
            .unconsumed_required
            .is_empty()
    );
}
