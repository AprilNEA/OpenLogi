//! The scripted Flow pairing, driven as the GUI and the console drive it.

use std::path::PathBuf;

use openlogi_core::config::FlowPeerName;
use openlogi_ipc::{
    FlowLinkState, FlowPairTarget, FlowPairingCommandError, FlowPairingFailure, FlowPairingPhase,
    FlowPlatform,
};
use tempfile::TempDir;

use super::super::control::{Command, parse};
use super::super::flow::{CODE, CONNECT_DELAY, FlowCommand, PEER_CONFIRM_DELAY};
use super::super::*;
use super::{demo_state, test_state};

/// A little past `delay`, so the step scheduled for it has run.
const STEP: Duration = Duration::from_millis(10);

struct Demo {
    agent: MockAgent,
    config: PathBuf,
    _dir: TempDir,
}

impl Demo {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temporary directory");
        let config = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        let mut state = demo_state();
        state.flow.config = Some(config.clone());
        Self {
            agent: MockAgent::new(state),
            config,
            _dir: dir,
        }
    }

    async fn phase(&self) -> Option<FlowPairingPhase> {
        self.agent.clone().snapshot(context()).await.flow.pairing
    }

    /// The names saved in `[[flow.peers]]`, with their addresses.
    fn saved(&self) -> Vec<(String, Vec<String>)> {
        let config = Config::load_from_path(&self.config).expect("the config loads");
        assert!(config.flow.layout().is_empty(), "pairing places no screen");
        config
            .flow
            .peers()
            .iter()
            .map(|peer| (peer.name.as_str().to_owned(), peer.addresses.clone()))
            .collect()
    }
}

fn context() -> tarpc::context::Context {
    tarpc::context::current()
}

#[tokio::test(start_paused = true)]
async fn the_scripted_pairing_saves_the_chosen_computer() {
    let demo = Demo::new();
    let agent = &demo.agent;
    agent
        .clone()
        .start_flow_pairing(context())
        .await
        .expect("open pairing");
    assert_eq!(demo.phase().await, Some(FlowPairingPhase::Searching));

    tokio::time::sleep(Duration::from_secs(3) + STEP).await;
    let Some(FlowPairingPhase::Found(nearby)) = demo.phase().await else {
        panic!("both nearby computers are found");
    };
    let names: Vec<_> = nearby.iter().map(|peer| peer.name.as_str()).collect();
    assert_eq!(names, ["Living Room iMac", "devbox"]);
    let devbox = nearby[1].clone();
    assert_eq!(devbox.platform, FlowPlatform::Linux);

    agent
        .clone()
        .pair_flow_peer(context(), FlowPairTarget::Nearby(devbox.public_key))
        .await
        .expect("pair");
    assert_eq!(
        demo.phase().await,
        Some(FlowPairingPhase::Connecting {
            peer: Some(devbox.clone())
        })
    );
    assert_eq!(
        agent
            .clone()
            .pair_flow_peer(context(), FlowPairTarget::Nearby(nearby[0].public_key))
            .await,
        Err(FlowPairingCommandError::Busy)
    );

    tokio::time::sleep(CONNECT_DELAY + STEP).await;
    let code = FlowPairingPhase::ShowingCode {
        peer: devbox.clone(),
        code: CODE.to_owned(),
    };
    assert_eq!(demo.phase().await, Some(code));
    agent
        .clone()
        .confirm_flow_pairing(context())
        .await
        .expect("confirm");
    assert!(matches!(
        demo.phase().await,
        Some(FlowPairingPhase::WaitingForPeer { .. })
    ));
    assert!(
        !demo.config.exists(),
        "nothing is saved before both confirm"
    );

    tokio::time::sleep(PEER_CONFIRM_DELAY + STEP).await;
    let Some(FlowPairingPhase::Paired { peer, name }) = demo.phase().await else {
        panic!("paired once the other user confirmed");
    };
    assert_eq!((peer, name.as_str()), (devbox.clone(), "devbox"));
    assert_eq!(demo.saved(), [("devbox".to_owned(), Vec::new())]);
    let flow = agent.clone().snapshot(context()).await.flow;
    assert_eq!(flow.peers.len(), 1);
    assert_eq!(flow.peers[0].public_key, devbox.public_key);
    assert_eq!(flow.peers[0].state, FlowLinkState::Lost);

    agent.clone().stop_flow_pairing(context()).await;
    assert_eq!(demo.phase().await, None);
}

#[tokio::test(start_paused = true)]
async fn the_console_scripts_the_other_computer() {
    let demo = Demo::new();
    let agent = &demo.agent;
    let console = |line: &str| match parse(line) {
        Ok(Some(Command::Flow(command))) => command,
        other => panic!("`{line}` parses as a flow command, not {other:?}"),
    };
    assert_eq!(
        console("flow fail timeout"),
        FlowCommand::Fail(FlowPairingFailure::TimedOut)
    );
    assert!(parse("flow fail").is_err() && parse("flow").is_err());
    assert!(
        agent.apply_flow(FlowCommand::Incoming).await.is_err(),
        "pairing is closed"
    );

    agent
        .clone()
        .start_flow_pairing(context())
        .await
        .expect("open pairing");
    agent
        .apply_flow(console("flow incoming"))
        .await
        .expect("another computer pairs");
    let Some(FlowPairingPhase::ShowingCode { peer: imac, .. }) = demo.phase().await else {
        panic!("an incoming attempt shows its code at once");
    };
    // The other user confirms first, so this side's confirm completes it.
    agent
        .apply_flow(console("flow confirm"))
        .await
        .expect("the other user confirms");
    agent
        .clone()
        .confirm_flow_pairing(context())
        .await
        .expect("confirm");
    assert_eq!(
        demo.phase().await,
        Some(FlowPairingPhase::Paired {
            peer: imac,
            name: FlowPeerName::new("Living Room iMac").expect("a valid name"),
        })
    );

    agent
        .clone()
        .cancel_flow_pairing(context())
        .await
        .expect("dismiss");
    agent
        .clone()
        .pair_flow_peer(
            context(),
            FlowPairTarget::Address("10.0.0.7:42425".to_owned()),
        )
        .await
        .expect("pair by address");
    assert_eq!(
        demo.phase().await,
        Some(FlowPairingPhase::Connecting { peer: None })
    );
    tokio::time::sleep(CONNECT_DELAY + STEP).await;
    agent
        .apply_flow(console("flow fail mismatch"))
        .await
        .expect("fail the attempt");
    let Some(FlowPairingPhase::Failed {
        peer: Some(office),
        reason: FlowPairingFailure::CodeMismatch,
    }) = demo.phase().await
    else {
        panic!("the attempt failed with the console's reason");
    };
    assert_eq!(office.platform, FlowPlatform::Windows);
    assert_eq!(
        agent.clone().confirm_flow_pairing(context()).await,
        Err(FlowPairingCommandError::NothingToConfirm)
    );
    assert_eq!(demo.saved(), [("Living Room iMac".to_owned(), Vec::new())]);
}

#[tokio::test]
async fn fixture_runs_do_not_open_flow_pairing() {
    let agent = MockAgent::new(test_state());
    assert!(matches!(
        agent.clone().start_flow_pairing(context()).await,
        Err(FlowPairingCommandError::Unavailable { .. })
    ));
    assert_eq!(
        agent
            .clone()
            .pair_flow_peer(context(), FlowPairTarget::Address("devbox".to_owned()))
            .await,
        Err(FlowPairingCommandError::NotStarted)
    );
    assert!(
        agent
            .clone()
            .read_flow_identity(context())
            .await
            .is_ok_and(|identity| !identity.machine_name.is_empty())
    );
}
