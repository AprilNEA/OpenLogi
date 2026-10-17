use super::*;

use std::{
    error::Error,
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use hidpp::{
    channel::{
        LONG_REPORT_ID, LONG_REPORT_LENGTH, RawHidChannel, SHORT_REPORT_ID, SHORT_REPORT_LENGTH,
    },
    protocol::v10::MessageType,
};

#[test]
fn receiver_family_follows_the_shared_protocol_registry() {
    for receiver in crate::RECEIVERS {
        let expected = match receiver.protocol {
            crate::ReceiverProtocol::Bolt => ReceiverFamily::Bolt,
            crate::ReceiverProtocol::Unifying => ReceiverFamily::Unifying,
        };
        assert_eq!(family_for(receiver.product_id), Some(expected));
    }
    assert_eq!(family_for(0xc5ff), None);
}

#[test]
fn passkey_clicks_are_msb_first_10_bits() {
    // 0b00_0000_0101 = 5 -> eight lefts then right, left, right.
    assert_eq!(
        passkey_to_clicks(5),
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

#[test]
fn session_state_keeps_bolt_phases_separate_from_unifying() {
    let mut device = DiscoveredDevice {
        address: [0xde, 0xad, 0xbe, 0xef, 0x01, 0x02],
        authentication: 0x01,
        kind: BoltDeviceKind::Keyboard,
        name: "Test Keyboard".into(),
    };

    let mut bolt = SessionState::from(ReceiverFamily::Bolt);
    let SessionState::BoltDiscovery(discovery) = &bolt else {
        panic!("Bolt must start in discovery");
    };
    assert!(discovery.partial.is_empty());

    bolt.select_bolt_device(&device)
        .expect("Bolt discovery accepts a device selection");
    let SessionState::BoltPairing(pairing) = &bolt else {
        panic!("device selection must enter Bolt pairing");
    };
    assert_eq!(pairing.authentication, 0x01);

    device.authentication = 0x00;
    bolt.select_bolt_device(&device)
        .expect("Bolt pairing preserves repeat-selection command acceptance");
    let SessionState::BoltPairing(pairing) = &bolt else {
        panic!("repeat selection must remain in Bolt pairing");
    };
    assert_eq!(pairing.authentication, 0x00);

    let mut unifying = SessionState::from(ReceiverFamily::Unifying);
    assert!(matches!(
        unifying.select_bolt_device(&device),
        Err(PairingError::UnsupportedCommand)
    ));
    assert!(matches!(unifying, SessionState::UnifyingOpening));
}

#[tokio::test]
async fn queued_discovery_notifications_after_selection_do_not_emit_device_found() {
    let (raw, mut written_reports) = EchoRawHidChannel::new();
    let Ok(channel) = HidppChannel::from_raw_channel(raw).await else {
        panic!("mock must support HID++");
    };
    let (command_tx, mut commands) = mpsc::unbounded_channel();
    let (notification_tx, mut notifications) = mpsc::unbounded_channel();
    let (event_tx, mut events) = mpsc::unbounded_channel();
    let late_address = [0xde, 0xad, 0xbe, 0xef, 0x01, 0x02];

    let exchange = async {
        for _ in 0..2 {
            written_reports
                .recv()
                .await
                .expect("notification and discovery setup reports");
        }
        assert!(matches!(events.recv().await, Some(PairingEvent::Searching)));

        command_tx
            .send(PairingCommand::Pair(DiscoveredDevice {
                address: late_address,
                authentication: 0x00,
                kind: BoltDeviceKind::Mouse,
                name: "Selected Mouse".into(),
            }))
            .expect("pair command");
        let pair = written_reports.recv().await.expect("Bolt pair report");
        assert_eq!(
            &pair[..5],
            &[
                LONG_REPORT_ID,
                RECEIVER_INDEX,
                u8::from(MessageType::SetLongRegister),
                BOLT_PAIRING,
                0x01,
            ]
        );

        notification_tx
            .send(discovery_info(7, late_address, 0x00))
            .expect("late discovery info");
        notification_tx
            .send(discovery_name(7, "Late Mouse"))
            .expect("late discovery name");
        notification_tx
            .send(pairing_succeeded(3))
            .expect("pairing completion");
    };

    let result = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(
            run_session(
                &channel,
                ReceiverFamily::Bolt,
                NotificationFlags::empty(),
                &mut commands,
                &mut notifications,
                &event_tx,
            ),
            exchange,
        )
    })
    .await;
    let Ok((result, ())) = result else {
        panic!("pairing session did not terminate");
    };
    result.expect("pairing completion must end the session successfully");
    assert!(matches!(
        events.recv().await,
        Some(PairingEvent::Paired { slot: 3 })
    ));
    assert!(
        events.try_recv().is_err(),
        "late discovery notifications must not emit DeviceFound during Bolt pairing"
    );
}

#[tokio::test]
async fn unifying_session_rejects_bolt_pair_command_before_writing_it() {
    let (raw, mut written_reports) = EchoRawHidChannel::new();
    let Ok(channel) = HidppChannel::from_raw_channel(raw).await else {
        panic!("mock must support HID++");
    };
    let (command_tx, mut commands) = mpsc::unbounded_channel();
    let (_notification_tx, mut notifications) = mpsc::unbounded_channel();
    let (event_tx, _events) = mpsc::unbounded_channel();

    assert!(
        command_tx
            .send(PairingCommand::Pair(DiscoveredDevice {
                address: [0xde, 0xad, 0xbe, 0xef, 0x01, 0x02],
                authentication: 0x01,
                kind: BoltDeviceKind::Keyboard,
                name: "Test Keyboard".into(),
            }))
            .is_ok(),
        "the pair command must reach the session's command receiver"
    );

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        run_session(
            &channel,
            ReceiverFamily::Unifying,
            NotificationFlags::empty(),
            &mut commands,
            &mut notifications,
            &event_tx,
        ),
    )
    .await;
    let Ok(result) = result else {
        panic!("pairing session did not terminate");
    };
    assert!(matches!(result, Err(PairingError::UnsupportedCommand)));

    let reports: Vec<_> = std::iter::from_fn(|| written_reports.try_recv().ok()).collect();
    assert_eq!(
        reports,
        vec![
            vec![
                SHORT_REPORT_ID,
                RECEIVER_INDEX,
                u8::from(MessageType::SetRegister),
                0x00, // notification-flags register
                0x00,
                0x09,
                0x00,
            ],
            vec![
                SHORT_REPORT_ID,
                RECEIVER_INDEX,
                u8::from(MessageType::SetRegister),
                UNIFYING_PAIRING,
                0x01,
                0x00,
                DISCOVERY_TIMEOUT,
            ],
            vec![
                SHORT_REPORT_ID,
                RECEIVER_INDEX,
                u8::from(MessageType::SetRegister),
                UNIFYING_PAIRING,
                0x02,
                0x00,
                0x00,
            ],
        ],
        "only notification setup, Unifying lock-open, and Unifying cancel may be written"
    );
}

#[tokio::test]
async fn malformed_passkey_after_pair_cancels_bolt_pairing() {
    let (raw, mut written_reports) = EchoRawHidChannel::new();
    let Ok(channel) = HidppChannel::from_raw_channel(raw).await else {
        panic!("mock must support HID++");
    };
    let (command_tx, mut commands) = mpsc::unbounded_channel();
    let (notification_tx, mut notifications) = mpsc::unbounded_channel();
    let (event_tx, _events) = mpsc::unbounded_channel();

    assert!(
        command_tx
            .send(PairingCommand::Pair(DiscoveredDevice {
                address: [0xde, 0xad, 0xbe, 0xef, 0x01, 0x02],
                authentication: 0x01,
                kind: BoltDeviceKind::Keyboard,
                name: "Test Keyboard".into(),
            }))
            .is_ok(),
        "the pair command must reach the session's command receiver"
    );

    let exchange = async {
        let mut reports = Vec::with_capacity(4);
        for _ in 0..3 {
            let Some(report) = written_reports.recv().await else {
                panic!("mock channel closed before pair command");
            };
            reports.push(report);
        }
        let mut data = [0u8; LONG_REPORT_LENGTH - 1];
        data[0] = RECEIVER_INDEX;
        data[1] = notification::id::PASSKEY_REQUEST;
        data[3..9].copy_from_slice(b"12x456");
        assert!(
            notification_tx.send(HidppMessage::Long(data)).is_ok(),
            "the malformed passkey notification must reach the running session"
        );

        let Some(cancel) = written_reports.recv().await else {
            panic!("mock channel closed before cancel command");
        };
        reports.push(cancel);
        reports
    };

    let result = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(
            run_session(
                &channel,
                ReceiverFamily::Bolt,
                NotificationFlags::empty(),
                &mut commands,
                &mut notifications,
                &event_tx,
            ),
            exchange,
        )
    })
    .await;
    let Ok((result, reports)) = result else {
        panic!("pairing session did not terminate");
    };

    assert!(matches!(
        result,
        Err(PairingError::MalformedNotification("passkey digits"))
    ));
    assert_eq!(reports.len(), 4);
    assert_eq!(
        &reports[2][..5],
        &[
            LONG_REPORT_ID,
            RECEIVER_INDEX,
            u8::from(MessageType::SetLongRegister),
            BOLT_PAIRING,
            0x01,
        ]
    );
    assert_eq!(
        &reports[3][..5],
        &[
            LONG_REPORT_ID,
            RECEIVER_INDEX,
            u8::from(MessageType::SetLongRegister),
            BOLT_PAIRING,
            0x02,
        ]
    );
    assert!(reports[3][5..].iter().all(|byte| *byte == 0));
}

#[tokio::test]
async fn failed_bolt_pair_write_cancels_bolt_pairing() {
    let (raw, mut written_reports) = EchoRawHidChannel::failing_bolt_pair_write();
    let Ok(channel) = HidppChannel::from_raw_channel(raw).await else {
        panic!("mock must support HID++");
    };
    let (command_tx, mut commands) = mpsc::unbounded_channel();
    let (_notification_tx, mut notifications) = mpsc::unbounded_channel();
    let (event_tx, _events) = mpsc::unbounded_channel();

    assert!(
        command_tx
            .send(PairingCommand::Pair(DiscoveredDevice {
                address: [0xde, 0xad, 0xbe, 0xef, 0x01, 0x02],
                authentication: 0x01,
                kind: BoltDeviceKind::Keyboard,
                name: "Test Keyboard".into(),
            }))
            .is_ok(),
        "the pair command must reach the session's command receiver"
    );

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        run_session(
            &channel,
            ReceiverFamily::Bolt,
            NotificationFlags::empty(),
            &mut commands,
            &mut notifications,
            &event_tx,
        ),
    )
    .await;
    let Ok(result) = result else {
        panic!("pairing session did not terminate");
    };
    assert!(matches!(result, Err(PairingError::Register(_))));

    let reports: Vec<_> = std::iter::from_fn(|| written_reports.try_recv().ok()).collect();
    assert_eq!(reports.len(), 4);
    assert_eq!(
        &reports[2][..5],
        &[
            LONG_REPORT_ID,
            RECEIVER_INDEX,
            u8::from(MessageType::SetLongRegister),
            BOLT_PAIRING,
            0x01,
        ],
        "the selected device is submitted to the Bolt pairing register"
    );
    assert_eq!(
        &reports[3][..5],
        &[
            LONG_REPORT_ID,
            RECEIVER_INDEX,
            u8::from(MessageType::SetLongRegister),
            BOLT_PAIRING,
            0x02,
        ],
        "a failed pair write must cancel the Bolt pairing register, not discovery"
    );
    assert!(reports[3][5..].iter().all(|byte| *byte == 0));
}

/// The register writes a Unifying session makes: notification flags, then the
/// lock-open request.
fn unifying_setup_reports() -> Vec<Vec<u8>> {
    vec![
        vec![
            SHORT_REPORT_ID,
            RECEIVER_INDEX,
            u8::from(MessageType::SetRegister),
            0x00, // notification-flags register
            0x00,
            0x09,
            0x00,
        ],
        vec![
            SHORT_REPORT_ID,
            RECEIVER_INDEX,
            u8::from(MessageType::SetRegister),
            UNIFYING_PAIRING,
            0x01,
            0x00,
            DISCOVERY_TIMEOUT,
        ],
    ]
}

/// The Unifying lock-close write a failed session ends with.
fn unifying_close_report() -> Vec<u8> {
    vec![
        SHORT_REPORT_ID,
        RECEIVER_INDEX,
        u8::from(MessageType::SetRegister),
        UNIFYING_PAIRING,
        0x02,
        0x00,
        0x00,
    ]
}

/// Runs a Unifying session against notifications already queued, returning
/// its result, the events it sent, every register write it made, and how long
/// it ran. `HidppChannel` reads on an OS thread, so a paused tokio clock would
/// auto-advance past its reply timeouts; these tests use the real clock.
async fn run_unifying_session(
    queued: Vec<HidppMessage>,
) -> (
    Result<(), PairingError>,
    Vec<PairingEvent>,
    Vec<Vec<u8>>,
    Duration,
) {
    let (raw, mut written_reports) = EchoRawHidChannel::new();
    let Ok(channel) = HidppChannel::from_raw_channel(raw).await else {
        panic!("mock must support HID++");
    };
    let (_command_tx, mut commands) = mpsc::unbounded_channel();
    let (notification_tx, mut notifications) = mpsc::unbounded_channel();
    let (event_tx, mut events) = mpsc::unbounded_channel();
    for message in queued {
        notification_tx.send(message).expect("queue a notification");
    }
    let started = tokio::time::Instant::now();
    let result = tokio::time::timeout(
        LOCK_CONFIRMATION_TIMEOUT * 5,
        run_session(
            &channel,
            ReceiverFamily::Unifying,
            NotificationFlags::empty(),
            &mut commands,
            &mut notifications,
            &event_tx,
        ),
    )
    .await
    .expect("pairing session did not terminate");
    let elapsed = started.elapsed();
    let events = std::iter::from_fn(|| events.try_recv().ok()).collect();
    let reports = std::iter::from_fn(|| written_reports.try_recv().ok()).collect();
    (result, events, reports, elapsed)
}

#[tokio::test]
async fn unifying_reports_searching_only_once_the_receiver_confirms_the_lock() {
    let (raw, mut written_reports) = EchoRawHidChannel::new();
    let Ok(channel) = HidppChannel::from_raw_channel(raw).await else {
        panic!("mock must support HID++");
    };
    let (_command_tx, mut commands) = mpsc::unbounded_channel();
    let (notification_tx, mut notifications) = mpsc::unbounded_channel();
    let (event_tx, mut events) = mpsc::unbounded_channel();
    let started = tokio::time::Instant::now();

    // `event_tx` outlives the session, so a session that fails before
    // reporting would leave `events.recv()` waiting forever. Bound the whole
    // exchange so a regression fails instead of hanging.
    tokio::time::timeout(LOCK_CONFIRMATION_TIMEOUT * 5, async {
        let exchange = async {
            for _ in 0..2 {
                written_reports
                    .recv()
                    .await
                    .expect("notification setup and lock-open writes");
            }
            // The lock-open write is acknowledged; the receiver has not said the
            // lock is open yet, so the UI must not be told to start searching.
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(
                events.try_recv().is_err(),
                "an acknowledged lock-open write alone must not report Searching"
            );
            notification_tx
                .send(unifying_lock(true, 0))
                .expect("lock-open notification");
            assert!(matches!(events.recv().await, Some(PairingEvent::Searching)));
            // Past the confirmation window: once confirmed, the session is
            // pairing and that window no longer applies.
            tokio::time::sleep_until(
                started + LOCK_CONFIRMATION_TIMEOUT + Duration::from_millis(500),
            )
            .await;
            notification_tx
                .send(device_connection(2, true))
                .expect("device connection");
        };

        let (result, ()) = tokio::join!(
            run_session(
                &channel,
                ReceiverFamily::Unifying,
                NotificationFlags::empty(),
                &mut commands,
                &mut notifications,
                &event_tx,
            ),
            exchange,
        );
        result.expect("a connection after the lock opened pairs the device");
        assert!(matches!(
            events.recv().await,
            Some(PairingEvent::Paired { slot: 2 })
        ));
    })
    .await
    .expect("the pairing exchange did not finish");
}

#[tokio::test]
async fn an_unconfirmed_unifying_lock_fails_the_session_and_closes_the_lock() {
    // A connection notification before the lock opened is an already-paired
    // device reconnecting, not a pairing.
    let (result, events, reports, elapsed) =
        run_unifying_session(vec![device_connection(1, true)]).await;
    assert!(
        matches!(&result, Err(PairingError::Register(message)) if message.contains("pairing lock")),
        "{result:?}"
    );
    assert!(events.is_empty(), "nothing may be reported: {events:?}");
    let mut expected = unifying_setup_reports();
    expected.push(unifying_close_report());
    assert_eq!(reports, expected);
    // The confirmation window ended it, not the 90 s session guard.
    assert!(elapsed >= LOCK_CONFIRMATION_TIMEOUT, "{elapsed:?}");
    assert!(elapsed < LOCK_CONFIRMATION_TIMEOUT * 2, "{elapsed:?}");
}

#[tokio::test]
async fn a_refused_unifying_lock_fails_the_session_at_once() {
    // An error code is the receiver's own reason, e.g. 0x03 too many devices.
    let (result, events, reports, elapsed) =
        run_unifying_session(vec![unifying_lock(false, 0x03)]).await;
    assert!(
        matches!(result, Err(PairingError::Device(0x03))),
        "{result:?}"
    );
    assert!(events.is_empty(), "{events:?}");
    assert_eq!(reports.last(), Some(&unifying_close_report()));
    assert!(elapsed < LOCK_CONFIRMATION_TIMEOUT, "{elapsed:?}");

    // Closed without ever opening and without a code.
    let (result, events, reports, elapsed) =
        run_unifying_session(vec![unifying_lock(false, 0)]).await;
    assert!(
        matches!(result, Err(PairingError::Register(_))),
        "{result:?}"
    );
    assert!(events.is_empty(), "{events:?}");
    assert_eq!(reports.last(), Some(&unifying_close_report()));
    assert!(elapsed < LOCK_CONFIRMATION_TIMEOUT, "{elapsed:?}");
}

#[tokio::test]
async fn a_lock_confirmation_that_beat_the_write_ack_still_counts() {
    // The listener is installed before the write, so a 0x4a that arrives
    // ahead of the ACK is queued and read once the session starts waiting.
    let (result, events, reports, _) =
        run_unifying_session(vec![unifying_lock(true, 0), device_connection(4, true)]).await;
    result.expect("the queued confirmation opens the session");
    assert!(
        matches!(
            events.as_slice(),
            [PairingEvent::Searching, PairingEvent::Paired { slot: 4 }]
        ),
        "{events:?}"
    );
    assert_eq!(reports, unifying_setup_reports());
}

/// A Unifying `0x4a` lock-state notification.
fn unifying_lock(open: bool, error: u8) -> HidppMessage {
    let mut data = [0u8; SHORT_REPORT_LENGTH - 1];
    data[0] = RECEIVER_INDEX;
    data[1] = notification::id::UNIFYING_LOCK;
    data[2] = u8::from(open);
    data[3] = error;
    HidppMessage::Short(data)
}

/// A `0x41` device-connection notification for `slot`.
fn device_connection(slot: u8, established: bool) -> HidppMessage {
    let mut data = [0u8; SHORT_REPORT_LENGTH - 1];
    data[0] = slot;
    data[1] = notification::id::DEVICE_CONNECTION;
    data[2] = 0x04; // Unifying protocol
    data[3] = if established { 0x02 } else { 0x42 }; // mouse, bit 6 = offline
    data[4..6].copy_from_slice(&0x4069u16.to_le_bytes());
    HidppMessage::Short(data)
}

fn discovery_info(counter: u16, address: [u8; 6], authentication: u8) -> HidppMessage {
    let mut data = [0u8; LONG_REPORT_LENGTH - 1];
    let [counter_low, counter_high] = counter.to_le_bytes();
    data[0] = RECEIVER_INDEX;
    data[1] = notification::id::DEVICE_DISCOVERY;
    data[2] = counter_low;
    data[3] = counter_high;
    data[4] = 0x00;
    data[6] = 0x02;
    data[9..15].copy_from_slice(&address);
    data[17] = authentication;
    HidppMessage::Long(data)
}

fn discovery_name(counter: u16, name: &str) -> HidppMessage {
    let mut data = [0u8; LONG_REPORT_LENGTH - 1];
    let [counter_low, counter_high] = counter.to_le_bytes();
    data[0] = RECEIVER_INDEX;
    data[1] = notification::id::DEVICE_DISCOVERY;
    data[2] = counter_low;
    data[3] = counter_high;
    data[4] = 0x01;
    data[5] = u8::try_from(name.len()).expect("test device name length fits in one byte");
    data[6..6 + name.len()].copy_from_slice(name.as_bytes());
    HidppMessage::Long(data)
}

fn pairing_succeeded(slot: u8) -> HidppMessage {
    let mut data = [0u8; LONG_REPORT_LENGTH - 1];
    data[0] = RECEIVER_INDEX;
    data[1] = notification::id::PAIRING_STATUS;
    data[2] = 0x02;
    data[10] = slot;
    HidppMessage::Long(data)
}

struct EchoRawHidChannel {
    incoming_tx: mpsc::UnboundedSender<Vec<u8>>,
    incoming_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    written_tx: mpsc::UnboundedSender<Vec<u8>>,
    fail_bolt_pair_write: AtomicBool,
}

impl EchoRawHidChannel {
    fn new() -> (Self, mpsc::UnboundedReceiver<Vec<u8>>) {
        Self::with_failed_bolt_pair_write(false)
    }

    fn failing_bolt_pair_write() -> (Self, mpsc::UnboundedReceiver<Vec<u8>>) {
        Self::with_failed_bolt_pair_write(true)
    }

    fn with_failed_bolt_pair_write(
        fail_bolt_pair_write: bool,
    ) -> (Self, mpsc::UnboundedReceiver<Vec<u8>>) {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (written_tx, written_rx) = mpsc::unbounded_channel();
        (
            Self {
                incoming_tx,
                incoming_rx: tokio::sync::Mutex::new(incoming_rx),
                written_tx,
                fail_bolt_pair_write: AtomicBool::new(fail_bolt_pair_write),
            },
            written_rx,
        )
    }
}

#[hidpp::async_trait]
impl RawHidChannel for EchoRawHidChannel {
    fn vendor_id(&self) -> u16 {
        0x046d
    }

    fn product_id(&self) -> u16 {
        0xc548
    }

    async fn write_report(&self, src: &[u8]) -> Result<usize, Box<dyn Error + Sync + Send>> {
        let report = src.to_vec();
        if self.written_tx.send(report.clone()).is_err() {
            return Err(mock_error());
        }
        if report.get(..5)
            == Some(
                &[
                    LONG_REPORT_ID,
                    RECEIVER_INDEX,
                    u8::from(MessageType::SetLongRegister),
                    BOLT_PAIRING,
                    0x01,
                ][..],
            )
            && self.fail_bolt_pair_write.swap(false, Ordering::Relaxed)
        {
            return Err(mock_error());
        }
        if self.incoming_tx.send(report).is_err() {
            return Err(mock_error());
        }
        Ok(src.len())
    }

    async fn read_report(&self, buf: &mut [u8]) -> Result<usize, Box<dyn Error + Sync + Send>> {
        let Some(report) = self.incoming_rx.lock().await.recv().await else {
            return Err(mock_error());
        };
        let len = report.len().min(buf.len());
        buf[..len].copy_from_slice(&report[..len]);
        Ok(len)
    }

    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)> {
        Some((true, true))
    }

    async fn get_report_descriptor(
        &self,
        _buf: &mut [u8],
    ) -> Result<usize, Box<dyn Error + Sync + Send>> {
        unreachable!("mock declares HID++ support")
    }
}

fn mock_error() -> Box<dyn Error + Sync + Send> {
    Box::new(io::Error::other("mock channel closed"))
}
