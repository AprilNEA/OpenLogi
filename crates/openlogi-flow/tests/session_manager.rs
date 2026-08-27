#![expect(
    clippy::tests_outside_test_module,
    reason = "Cargo integration tests are test-only crates"
)]

use std::{
    error::Error as StdError,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use openlogi_flow::{
    discovery::{CandidateFuture, CandidateSource, ManualCandidateSource},
    frame::FrameKind,
    generated as proto,
    sas::PublicKey,
    session::{
        LinkState, PeerConfig, PeerSessionHandle, SessionManager, SessionPolicy,
        TrustedInitialState, TrustedStateProvider,
    },
    transport::{
        ConnectionDirection, FlowConnection, FlowEndpoint, MachineIdentity, NotificationEvent,
        PeerTrust,
    },
};
use tokio::sync::Barrier;

type TestResult<T = ()> = Result<T, Box<dyn StdError + Send + Sync>>;

#[derive(Debug)]
struct CountingProvider {
    calls: AtomicUsize,
    revision: u64,
}

impl CountingProvider {
    fn new(revision: u64) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            revision,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }
}

impl TrustedStateProvider for CountingProvider {
    fn initial_state(&self, _peer_key: PublicKey) -> TrustedInitialState {
        self.calls.fetch_add(1, Ordering::AcqRel);
        TrustedInitialState {
            announce_devices: proto::AnnounceDevices {
                revision: self.revision,
                ..Default::default()
            },
            peer_state: proto::PeerState {
                flow_enabled: true,
                revision: self.revision,
                ..Default::default()
            },
        }
    }
}

struct BarrierSource {
    address: SocketAddr,
    barrier: Arc<Barrier>,
}

impl CandidateSource for BarrierSource {
    fn candidates(&self, _peer_key: PublicKey) -> CandidateFuture<'_> {
        Box::pin(async move {
            self.barrier.wait().await;
            Ok(vec![self.address])
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn managers_establish_a_session_and_send_trusted_initial_state() -> TestResult {
    let first_identity = MachineIdentity::generate()?;
    let second_identity = MachineIdentity::generate()?;
    let first = endpoint(&first_identity, second_identity.public_key(), [1; 16])?;
    let second = endpoint(&second_identity, first_identity.public_key(), [2; 16])?;
    let first_provider = Arc::new(CountingProvider::new(11));
    let second_provider = Arc::new(CountingProvider::new(22));

    let first_manager = SessionManager::start(
        Arc::clone(&first),
        [peer_with_address(
            second_identity.public_key(),
            second.local_addr()?,
        )],
        first_provider.clone(),
        test_policy(),
    )?;
    let second_manager = SessionManager::start(
        Arc::clone(&second),
        [incoming_only_peer(first_identity.public_key())],
        second_provider.clone(),
        test_policy(),
    )?;
    let first_handle = first_manager
        .peer(second_identity.public_key())
        .ok_or("first manager omitted its peer handle")?
        .clone();
    let second_handle = second_manager
        .peer(first_identity.public_key())
        .ok_or("second manager omitted its peer handle")?
        .clone();

    wait_for_state(&first_handle, LinkState::Connected).await?;
    wait_for_state(&second_handle, LinkState::Connected).await?;
    let second_connection = wait_for_connection(&second_handle).await?;
    assert_eq!(
        next_notification_kind(&second_connection).await?,
        FrameKind::AnnounceDevices
    );
    assert_eq!(
        next_notification_kind(&second_connection).await?,
        FrameKind::PeerState
    );
    wait_for_calls(&first_provider, 1).await?;
    wait_for_calls(&second_provider, 1).await?;
    assert_eq!(first_provider.calls(), 1);
    assert_eq!(second_provider.calls(), 1);

    first_manager.shutdown().await;
    second_manager.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_dial_converges_on_the_key_order_tiebreak() -> TestResult {
    let first_identity = MachineIdentity::generate()?;
    let second_identity = MachineIdentity::generate()?;
    let first = endpoint(&first_identity, second_identity.public_key(), [7; 16])?;
    let second = endpoint(&second_identity, first_identity.public_key(), [8; 16])?;
    let barrier = Arc::new(Barrier::new(2));
    let first_manager = SessionManager::start(
        Arc::clone(&first),
        [barrier_peer(
            second_identity.public_key(),
            second.local_addr()?,
            Arc::clone(&barrier),
        )],
        Arc::new(CountingProvider::new(1)),
        test_policy(),
    )?;
    let second_manager = SessionManager::start(
        Arc::clone(&second),
        [barrier_peer(
            first_identity.public_key(),
            first.local_addr()?,
            barrier,
        )],
        Arc::new(CountingProvider::new(2)),
        test_policy(),
    )?;
    let first_handle = first_manager
        .peer(second_identity.public_key())
        .ok_or("first manager omitted its peer handle")?
        .clone();
    let second_handle = second_manager
        .peer(first_identity.public_key())
        .ok_or("second manager omitted its peer handle")?
        .clone();
    let first_direction = if first_identity.public_key() < second_identity.public_key() {
        ConnectionDirection::Incoming
    } else {
        ConnectionDirection::Outgoing
    };
    let second_direction = if second_identity.public_key() < first_identity.public_key() {
        ConnectionDirection::Incoming
    } else {
        ConnectionDirection::Outgoing
    };

    wait_for_direction(&first_handle, first_direction).await?;
    wait_for_direction(&second_handle, second_direction).await?;
    assert_eq!(first_handle.state(), LinkState::Connected);
    assert_eq!(second_handle.state(), LinkState::Connected);

    first_manager.shutdown().await;
    second_manager.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manager_reconnects_after_peer_session_is_dropped() -> TestResult {
    let first_identity = MachineIdentity::generate()?;
    let second_identity = MachineIdentity::generate()?;
    let first = endpoint(&first_identity, second_identity.public_key(), [3; 16])?;
    let second = endpoint(&second_identity, first_identity.public_key(), [4; 16])?;
    let first_provider = Arc::new(CountingProvider::new(1));
    let second_provider = Arc::new(CountingProvider::new(2));
    let first_manager = SessionManager::start(
        Arc::clone(&first),
        [peer_with_address(
            second_identity.public_key(),
            second.local_addr()?,
        )],
        first_provider.clone(),
        test_policy(),
    )?;
    let second_manager = SessionManager::start(
        Arc::clone(&second),
        [incoming_only_peer(first_identity.public_key())],
        second_provider.clone(),
        test_policy(),
    )?;
    let first_handle = first_manager
        .peer(second_identity.public_key())
        .ok_or("first manager omitted its peer handle")?
        .clone();
    wait_for_state(&first_handle, LinkState::Connected).await?;

    second_manager.shutdown().await;
    wait_for_state(&first_handle, LinkState::Lost).await?;

    let restarted_second_manager = SessionManager::start(
        Arc::clone(&second),
        [incoming_only_peer(first_identity.public_key())],
        second_provider.clone(),
        test_policy(),
    )?;
    let restarted_handle = restarted_second_manager
        .peer(first_identity.public_key())
        .ok_or("restarted manager omitted its peer handle")?
        .clone();
    wait_for_state(&first_handle, LinkState::Connected).await?;
    wait_for_state(&restarted_handle, LinkState::Connected).await?;
    wait_for_calls(&first_provider, 2).await?;

    first_manager.shutdown().await;
    restarted_second_manager.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_pong_degrades_then_transport_close_marks_lost() -> TestResult {
    let managed_identity = MachineIdentity::generate()?;
    let silent_identity = MachineIdentity::generate()?;
    let managed = endpoint(&managed_identity, silent_identity.public_key(), [5; 16])?;
    let silent = endpoint(&silent_identity, managed_identity.public_key(), [6; 16])?;
    let silent_acceptor = Arc::clone(&silent);
    let accepted = tokio::spawn(async move { silent_acceptor.accept().await });
    let provider = Arc::new(CountingProvider::new(1));
    let sessions = SessionManager::start(
        Arc::clone(&managed),
        [peer_with_address(
            silent_identity.public_key(),
            silent.local_addr()?,
        )],
        provider,
        test_policy(),
    )?;
    let handle = sessions
        .peer(silent_identity.public_key())
        .ok_or("manager omitted its peer handle")?
        .clone();
    let silent_connection = accepted.await??;

    wait_for_state(&handle, LinkState::Connected).await?;
    wait_for_state(&handle, LinkState::Degraded).await?;
    silent_connection.close();
    wait_for_state(&handle, LinkState::Lost).await?;

    sessions.shutdown().await;
    Ok(())
}

fn endpoint(
    identity: &MachineIdentity,
    peer_key: PublicKey,
    nonce: [u8; 16],
) -> TestResult<Arc<FlowEndpoint>> {
    Ok(Arc::new(FlowEndpoint::bind(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        identity.clone(),
        PeerTrust::pinned([peer_key]),
        proto::Hello {
            proto_min: 1,
            proto_max: 1,
            public_key: identity.public_key().as_bytes().to_vec(),
            session_nonce: nonce.to_vec(),
            machine_name: "session-test".to_owned(),
            app_version: "test".to_owned(),
            ..Default::default()
        },
    )?))
}

fn peer_with_address(public_key: PublicKey, address: SocketAddr) -> PeerConfig {
    let source: Arc<dyn CandidateSource> =
        Arc::new(ManualCandidateSource::new([address.to_string()]));
    PeerConfig {
        public_key,
        sources: vec![source],
    }
}

fn barrier_peer(public_key: PublicKey, address: SocketAddr, barrier: Arc<Barrier>) -> PeerConfig {
    PeerConfig {
        public_key,
        sources: vec![Arc::new(BarrierSource { address, barrier })],
    }
}

fn incoming_only_peer(public_key: PublicKey) -> PeerConfig {
    PeerConfig {
        public_key,
        sources: Vec::new(),
    }
}

fn test_policy() -> SessionPolicy {
    SessionPolicy {
        retry_base: Duration::from_millis(20),
        retry_max: Duration::from_millis(100),
        ping_interval: Duration::from_millis(25),
        degraded_after: Duration::from_millis(100),
    }
}

async fn wait_for_state(handle: &PeerSessionHandle, expected: LinkState) -> TestResult {
    let mut state = handle.subscribe_state();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if *state.borrow_and_update() == expected {
                return Ok::<_, Box<dyn StdError + Send + Sync>>(());
            }
            state
                .changed()
                .await
                .map_err(|_| "session state channel closed")?;
        }
    })
    .await
    .map_err(|_| format!("timed out waiting for {expected:?}"))??;
    Ok(())
}

async fn wait_for_connection(handle: &PeerSessionHandle) -> TestResult<Arc<FlowConnection>> {
    let mut connection = handle.subscribe_connection();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(connection) = connection.borrow_and_update().clone() {
                return Ok::<_, Box<dyn StdError + Send + Sync>>(connection);
            }
            connection
                .changed()
                .await
                .map_err(|_| "session connection channel closed")?;
        }
    })
    .await
    .map_err(|_| "timed out waiting for a live session connection")?
}

async fn wait_for_direction(
    handle: &PeerSessionHandle,
    expected: ConnectionDirection,
) -> TestResult {
    let mut connection = handle.subscribe_connection();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if connection
                .borrow_and_update()
                .as_ref()
                .is_some_and(|connection| connection.direction() == expected)
            {
                return Ok::<_, Box<dyn StdError + Send + Sync>>(());
            }
            connection
                .changed()
                .await
                .map_err(|_| "session connection channel closed")?;
        }
    })
    .await
    .map_err(|_| format!("timed out waiting for {expected:?} connection"))??;
    Ok(())
}

async fn next_notification_kind(connection: &FlowConnection) -> TestResult<FrameKind> {
    let event = tokio::time::timeout(Duration::from_secs(3), connection.accept_notification())
        .await
        .map_err(|_| "timed out waiting for initial state notification")??;
    match event {
        NotificationEvent::Notification(envelope) => Ok(envelope.kind),
        NotificationEvent::Dropped(code) => {
            Err(format!("initial state notification was dropped: {code:?}").into())
        }
    }
}

async fn wait_for_calls(provider: &CountingProvider, expected: usize) -> TestResult {
    tokio::time::timeout(Duration::from_secs(3), async {
        while provider.calls() < expected {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .map_err(|_| format!("timed out waiting for {expected} provider calls"))?;
    Ok(())
}
