//! Two or three pairing endpoints on loopback, without mDNS, each saving to
//! its own config file.

use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use openlogi_core::config::{Config, FlowPublicKey};
use openlogi_flow::frame::FrameKind;
use openlogi_flow::generated as proto;
use openlogi_flow::transport::{MachineIdentity, message_envelope};
use openlogi_ipc::{FlowPairTarget, FlowPairingCommandError, FlowPairingFailure, FlowPairingPhase};
use tempfile::TempDir;
use tokio::time::timeout;

use super::{FlowPairing, Setup, bind_pairing, flow_key};
use crate::observable::ObservableState;

const WAIT: Duration = Duration::from_secs(10);

struct Computer {
    pairing: FlowPairing,
    observable: Arc<ObservableState>,
    key: FlowPublicKey,
    address: String,
    config: PathBuf,
    _dir: TempDir,
}

impl Computer {
    /// A computer whose codes stay valid for `code_timeout` when it answers
    /// a pairing request.
    fn new(code_timeout: Option<Duration>) -> Self {
        let identity = MachineIdentity::generate().expect("identity");
        let key = flow_key(identity.public_key());
        let port = free_port();
        let dir = tempfile::tempdir().expect("temporary directory");
        let config = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        let observable = Arc::new(ObservableState::new("test".to_owned()));
        let pairing = FlowPairing::with_setup(
            Arc::clone(&observable),
            Setup {
                address: SocketAddr::from(([127, 0, 0, 1], port)),
                mdns: false,
                identity: Some(identity),
                config: Some(config.clone()),
                code_timeout,
            },
        );
        Self {
            pairing,
            observable,
            key,
            address: format!("127.0.0.1:{port}"),
            config,
            _dir: dir,
        }
    }

    async fn started(code_timeout: Option<Duration>) -> Self {
        let computer = Self::new(code_timeout);
        computer.pairing.start().await.expect("open pairing");
        computer
    }

    fn target(&self) -> FlowPairTarget {
        FlowPairTarget::Address(self.address.clone())
    }

    /// Wait until the published pairing phase satisfies `wanted`.
    async fn phase(&self, wanted: impl Fn(&FlowPairingPhase) -> bool) -> FlowPairingPhase {
        let mut observations = self.observable.subscribe();
        let observed = timeout(
            WAIT,
            observations
                .wait_for(|observed| observed.snapshot.flow.pairing.as_ref().is_some_and(&wanted)),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "pairing phase in time; last {:?}",
                self.observable.snapshot().flow.pairing
            )
        })
        .expect("the observable lives");
        observed.snapshot.flow.pairing.clone().expect("a phase")
    }

    async fn code(&self, peer: FlowPublicKey) -> String {
        match self
            .phase(|phase| matches!(phase, FlowPairingPhase::ShowingCode { .. }))
            .await
        {
            FlowPairingPhase::ShowingCode { peer: shown, code } => {
                assert_eq!(shown.public_key, peer);
                code
            }
            _ => unreachable!(),
        }
    }

    async fn failure(&self) -> FlowPairingFailure {
        match self
            .phase(|phase| matches!(phase, FlowPairingPhase::Failed { .. }))
            .await
        {
            FlowPairingPhase::Failed { reason, .. } => reason,
            _ => unreachable!(),
        }
    }

    /// The peers saved in this computer's config: key and addresses.
    fn saved(&self) -> Vec<(FlowPublicKey, Vec<String>)> {
        if !self.config.exists() {
            return Vec::new();
        }
        let config = Config::load_from_path(&self.config).expect("the saved config loads");
        assert!(config.flow.layout().is_empty(), "pairing places no screen");
        config
            .flow
            .peers()
            .iter()
            .map(|peer| (peer.public_key, peer.addresses.clone()))
            .collect()
    }
}

/// A loopback UDP port nothing listens on right now.
fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .and_then(|socket| socket.local_addr())
        .expect("a free port")
        .port()
}

/// Both computers show the same code, and each saves the other only once
/// both users confirmed, whichever confirms first.
#[tokio::test]
async fn two_computers_pair_and_save_each_other() {
    for initiator_confirms_first in [true, false] {
        let initiator = Computer::started(None).await;
        let responder = Computer::started(None).await;
        let mut saved = initiator.pairing.saved();

        initiator
            .pairing
            .pair(responder.target())
            .await
            .expect("pair");
        let code = initiator.code(responder.key).await;
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|digit| digit.is_ascii_digit()));
        assert_eq!(responder.code(initiator.key).await, code);

        let (first, second) = if initiator_confirms_first {
            (&initiator, &responder)
        } else {
            (&responder, &initiator)
        };
        first.pairing.confirm().await.expect("confirm");
        first
            .phase(|phase| matches!(phase, FlowPairingPhase::WaitingForPeer { .. }))
            .await;
        assert!(first.saved().is_empty() && second.saved().is_empty());

        second.pairing.confirm().await.expect("confirm");
        for (computer, peer) in [(&initiator, &responder), (&responder, &initiator)] {
            let paired = computer
                .phase(|phase| matches!(phase, FlowPairingPhase::Paired { .. }))
                .await;
            let FlowPairingPhase::Paired { peer: shown, .. } = paired else {
                unreachable!()
            };
            assert_eq!(shown.public_key, peer.key);
        }
        // Only the computer that dialed knows an address for the other.
        assert_eq!(
            initiator.saved(),
            [(responder.key, vec!["127.0.0.1".to_owned()])]
        );
        assert_eq!(responder.saved(), [(initiator.key, Vec::new())]);
        assert!(saved.has_changed().expect("pairing lives"));
        saved.mark_unchanged();

        initiator.pairing.stop().await;
        responder.pairing.stop().await;
        assert_eq!(initiator.observable.snapshot().flow.pairing, None);
    }
}

/// Cancelling shows the other computer that this one cancelled, and saves
/// nothing on either.
#[tokio::test]
async fn cancelling_tells_the_other_computer() {
    let initiator = Computer::started(None).await;
    let responder = Computer::started(None).await;
    initiator
        .pairing
        .pair(responder.target())
        .await
        .expect("pair");
    initiator.code(responder.key).await;
    responder.code(initiator.key).await;

    responder.pairing.cancel().await.expect("cancel");
    assert_eq!(initiator.failure().await, FlowPairingFailure::PeerCancelled);
    responder
        .phase(|phase| *phase == FlowPairingPhase::Searching)
        .await;
    assert!(initiator.saved().is_empty() && responder.saved().is_empty());

    // Dismissing the result lists the nearby computers again.
    initiator.pairing.cancel().await.expect("dismiss");
    initiator
        .phase(|phase| *phase == FlowPairingPhase::Searching)
        .await;
}

/// The computer that answers decides how long the code stays valid; when
/// nobody confirms in time, both computers say so.
#[tokio::test]
async fn an_unconfirmed_code_times_out_on_both_computers() {
    let initiator = Computer::started(None).await;
    let responder = Computer::started(Some(Duration::from_millis(500))).await;
    initiator
        .pairing
        .pair(responder.target())
        .await
        .expect("pair");
    initiator.code(responder.key).await;
    initiator.pairing.confirm().await.expect("confirm");

    assert_eq!(initiator.failure().await, FlowPairingFailure::TimedOut);
    assert_eq!(responder.failure().await, FlowPairingFailure::TimedOut);
    assert!(initiator.saved().is_empty() && responder.saved().is_empty());
}

/// A computer already pairing turns a third one away, and an address with
/// nobody behind it fails without waiting for a code.
#[tokio::test]
async fn busy_and_unreachable_computers_fail_the_attempt() {
    let initiator = Computer::started(None).await;
    let responder = Computer::started(None).await;
    let third = Computer::started(None).await;
    initiator
        .pairing
        .pair(responder.target())
        .await
        .expect("pair");
    responder.code(initiator.key).await;

    third.pairing.pair(responder.target()).await.expect("pair");
    assert_eq!(third.failure().await, FlowPairingFailure::PeerBusy);
    // The attempt in progress goes on.
    assert!(matches!(
        responder.observable.snapshot().flow.pairing,
        Some(FlowPairingPhase::ShowingCode { .. })
    ));

    third
        .pairing
        .pair(FlowPairTarget::Address("127.0.0.1:0".to_owned()))
        .await
        .expect("a new attempt after a failed one");
    assert_eq!(third.failure().await, FlowPairingFailure::Unreachable);
}

/// An attempt that ended before its ceremony started refuses what the
/// peer asks while its connection lingers: a late `PairStart` no longer
/// brings back an attempt its user could neither confirm nor cancel, and
/// the linger, not a code's validity, closes the connection.
#[tokio::test]
async fn an_ended_attempt_refuses_a_late_pair_start() {
    let responder = Computer::started(None).await;
    let identity = MachineIdentity::generate().expect("identity");
    let endpoint =
        bind_pairing(SocketAddr::from(([127, 0, 0, 1], 0)), identity).expect("a pairing endpoint");
    let address = responder.address.parse().expect("a socket address");
    let peer = endpoint
        .dial(address)
        .await
        .expect("dial")
        .establish()
        .await
        .expect("handshake");

    // A reveal before any `PairStart` ends the responder's attempt.
    let reveal = proto::PairReveal {
        nonce: vec![0; 16],
        ..Default::default()
    };
    let reveal = message_envelope(FrameKind::PairReveal, &reveal).expect("PairReveal");
    let refused = peer.call(reveal).await.expect("an answer");
    assert_eq!(refused.kind, FrameKind::Error);

    let start = proto::PairStart {
        nonce_commitment: vec![0; 32],
        ..Default::default()
    };
    let start = message_envelope(FrameKind::PairStart, &start).expect("PairStart");
    let late = peer.call(start).await.expect("an answer");
    assert_eq!(late.kind, FrameKind::Error);

    let closed = timeout(WAIT, peer.accept_rpc_stream())
        .await
        .expect("the linger closes the connection");
    let Err(_) = closed else {
        panic!("the responder opened a request stream");
    };
    assert_eq!(
        responder.observable.snapshot().flow.pairing,
        Some(FlowPairingPhase::Searching)
    );
    assert!(responder.saved().is_empty());
}

#[tokio::test]
async fn commands_need_open_pairing_and_a_code() {
    let computer = Computer::new(None);
    assert_eq!(
        computer.pairing.pair(computer.target()).await,
        Err(FlowPairingCommandError::NotStarted)
    );
    computer.pairing.start().await.expect("open pairing");
    assert_eq!(
        computer.observable.snapshot().flow.pairing,
        Some(FlowPairingPhase::Searching)
    );
    assert_eq!(
        computer.pairing.confirm().await,
        Err(FlowPairingCommandError::NothingToConfirm)
    );
    let identity = computer.pairing.identity().await.expect("identity");
    assert_eq!(identity.public_key, computer.key);

    computer.pairing.stop().await;
    assert_eq!(computer.observable.snapshot().flow.pairing, None);
    assert_eq!(
        computer.pairing.cancel().await,
        Err(FlowPairingCommandError::NotStarted)
    );
    // Pairing opens again on the port it just closed.
    computer.pairing.start().await.expect("open pairing again");
}
