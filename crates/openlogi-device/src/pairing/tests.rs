use super::*;

use std::{error::Error, io, time::Duration};

use hidpp::{
    channel::{LONG_REPORT_ID, LONG_REPORT_LENGTH, RawHidChannel, SHORT_REPORT_ID},
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
                NOTIFICATIONS,
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

struct EchoRawHidChannel {
    incoming_tx: mpsc::UnboundedSender<Vec<u8>>,
    incoming_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    written_tx: mpsc::UnboundedSender<Vec<u8>>,
}

impl EchoRawHidChannel {
    fn new() -> (Self, mpsc::UnboundedReceiver<Vec<u8>>) {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (written_tx, written_rx) = mpsc::unbounded_channel();
        (
            Self {
                incoming_tx,
                incoming_rx: tokio::sync::Mutex::new(incoming_rx),
                written_tx,
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
        if self.written_tx.send(report.clone()).is_err() || self.incoming_tx.send(report).is_err() {
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
