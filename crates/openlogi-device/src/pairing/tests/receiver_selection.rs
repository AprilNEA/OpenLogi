use openlogi_fixture::{
    CassetteExchange, FIXTURE_SCHEMA_VERSION, HidCassette, ReportSupport, RequestMatch,
};

use crate::replay::{
    ChannelConnection, NodePresence, OpenOutcome, RawWriterAvailability, ReplayBackend,
    ReplayChannel, ReplayNode, ReplayTopology,
};
use crate::{NodeId, NodeInfo};

use super::super::{PairingError, ReceiverFamily, ReceiverSelector, open_receiver};

const BOLT_A: &str = "00000000AAAABBBB";
const BOLT_B: &str = "00000000CCCCDDDD";

fn backend(tag: &str) -> ReplayBackend {
    let mut nodes = Vec::new();
    let mut channels = Vec::new();
    let mut cassettes = Vec::new();
    for (name, product_id, uid) in [
        ("unifying", 0xc52b, "11223344"),
        ("bolt-a", 0xc548, BOLT_A),
        ("bolt-b", 0xc548, BOLT_B),
    ] {
        nodes.push(ReplayNode {
            info: NodeInfo {
                id: node_id(tag, name),
                vendor_id: 0x046d,
                product_id,
                usage_page: 0xff00,
                usage_id: 2,
                name: name.into(),
                manufacturer: None,
                serial_number: None,
            },
            presence: NodePresence::Present,
            open_outcome: OpenOutcome::Hidpp,
            channel: Some(name.into()),
            raw_writer: RawWriterAvailability::Unavailable,
            receiver_slots: Vec::new(),
        });
        channels.push(ReplayChannel {
            id: name.into(),
            connection: ChannelConnection::Connected,
            report_support: ReportSupport::ShortAndLong,
        });
        let (register, subregister) = if product_id == 0xc548 {
            (0xfb, 0)
        } else {
            (0xb5, 3)
        };
        let mut response = vec![0x11, 0xff, 0x83, register];
        if product_id == 0xc548 {
            response.extend_from_slice(uid.as_bytes());
        } else {
            response.extend_from_slice(&[3, 0x11, 0x22, 0x33, 0x44, 0, 6]);
            response.resize(20, 0);
        }
        cassettes.push(HidCassette {
            schema_version: FIXTURE_SCHEMA_VERSION,
            name: name.into(),
            channel: name.into(),
            report_support: ReportSupport::ShortAndLong,
            exchanges: vec![CassetteExchange {
                request: vec![0x10, 0xff, 0x83, register, subregister, 0, 0],
                response: Some(response),
                request_match: RequestMatch::Exact,
                required: true,
            }],
        });
    }
    ReplayBackend::new(ReplayTopology { nodes, channels }, cassettes)
        .expect("valid receiver topology")
}

fn node_id(tag: &str, name: &str) -> NodeId {
    NodeId::from(format!(
        "pairing-selection-{}-{tag}-{name}",
        std::process::id()
    ))
}

fn target(product_id: u16, uid: &str) -> ReceiverSelector {
    ReceiverSelector::ReceiverUid {
        product_id,
        uid: uid.into(),
    }
}

#[tokio::test]
async fn selects_second_bolt_by_uid_even_with_unifying_first() {
    let backend = backend("second-bolt");
    let opened = open_receiver(&backend, &target(0xc548, &BOLT_B.to_ascii_lowercase()))
        .await
        .expect("selected Bolt receiver opens");
    assert!(matches!(opened.family, ReceiverFamily::Bolt));
    assert_eq!(
        backend
            .open_count(&node_id("second-bolt", "unifying"))
            .unwrap(),
        0
    );
    for channel in ["bolt-a", "bolt-b"] {
        assert_eq!(
            backend
                .channel_completion(channel)
                .unwrap()
                .written_reports
                .len(),
            1
        );
    }
    assert_eq!(backend.channel_lifetime_count("bolt-a").unwrap(), 0);
    assert_eq!(backend.channel_lifetime_count("bolt-b").unwrap(), 1);
}

#[tokio::test]
async fn selects_unifying_by_its_serial_without_opening_bolt() {
    let backend = backend("unifying");
    let opened = open_receiver(&backend, &target(0xc52b, "11223344"))
        .await
        .expect("selected Unifying receiver opens");
    assert!(matches!(opened.family, ReceiverFamily::Unifying));
    assert_eq!(
        backend.open_count(&node_id("unifying", "bolt-a")).unwrap(),
        0
    );
    assert_eq!(
        backend.open_count(&node_id("unifying", "bolt-b")).unwrap(),
        0
    );
}

#[tokio::test]
async fn disconnected_selection_never_falls_back_to_another_receiver() {
    let backend = backend("missing");
    backend
        .set_node_presence(&node_id("missing", "bolt-b"), NodePresence::Absent)
        .unwrap();
    assert!(matches!(
        open_receiver(&backend, &target(0xc548, BOLT_B)).await,
        Err(PairingError::ReceiverNotFound)
    ));
    assert_eq!(
        backend.open_count(&node_id("missing", "unifying")).unwrap(),
        0
    );
    let completion = backend.channel_completion("bolt-a").unwrap();
    assert_eq!(
        completion.written_reports,
        vec![vec![0x10, 0xff, 0x83, 0xfb, 0, 0, 0]],
        "only an identity read is allowed on a different receiver"
    );
}
