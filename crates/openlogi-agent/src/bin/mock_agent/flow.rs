//! The mock's scripted Flow pairing, so the GUI's pairing window runs on one
//! computer.
//!
//! Opening pairing finds two nearby computers, one after the other. Pairing
//! with either, or with any typed address, connects and shows the code
//! [`CODE`]; once the user confirms, the other computer's user confirms a
//! few seconds later and the peer is saved in `config.toml` the way the
//! agent saves it, through `ConfigFile::save`. The snapshot lists the peers
//! saved there.
//!
//! The console's `flow` commands set a saved peer's link state, make another
//! computer start pairing with this one, make the other user confirm first,
//! or fail the attempt for any reason. Fixture runs keep test time frozen, so
//! pairing refuses to open there, as the Bolt script does.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use openlogi_agent_core::flow::{PairedPeer, save_peer};
use openlogi_core::config::{ConfigFile, FlowPublicKey};
use openlogi_ipc::{
    FlowIdentity, FlowLinkState, FlowPairTarget, FlowPairingCommandError, FlowPairingFailure,
    FlowPairingPeer, FlowPairingPhase, FlowPeerStatus, FlowPlatform, FlowStatus,
};

use super::MockAgent;

/// This computer's scripted Flow identity.
const MACHINE_NAME: &str = "OpenLogi Mock";
const MACHINE_KEY: [u8; 32] = [0x4f; 32];
/// The computers with pairing open nearby, in the order they appear, and
/// how long after pairing opens each does.
const NEARBY: [(&str, u8, FlowPlatform, Duration); 2] = [
    (
        "Living Room iMac",
        0x11,
        FlowPlatform::Macos,
        Duration::from_millis(1500),
    ),
    ("devbox", 0x22, FlowPlatform::Linux, Duration::from_secs(3)),
];
/// The computer that answers at any typed address.
const ADDRESS_PEER: (&str, u8, FlowPlatform) = ("Office PC", 0x33, FlowPlatform::Windows);
/// How long connecting takes before the code shows.
pub(super) const CONNECT_DELAY: Duration = Duration::from_millis(800);
/// How long the other computer's user takes to confirm after this one did.
pub(super) const PEER_CONFIRM_DELAY: Duration = Duration::from_secs(3);
/// The code both computers show.
pub(super) const CODE: &str = "418672";

/// A console `flow` command.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum FlowCommand {
    /// Report the link to the saved peer named `peer` as `state`.
    Link { peer: String, state: FlowLinkState },
    /// A nearby computer starts pairing with this one.
    Incoming,
    /// The other computer's user confirms the code.
    PeerConfirms,
    /// The attempt fails for `reason`.
    Fail(FlowPairingFailure),
}

/// Flow pairing as the mock scripts it.
#[derive(Debug, Default)]
pub(super) struct MockFlow {
    /// Where paired computers are saved, and where the snapshot reads
    /// them; `None` for the user's config file. Tests set their own.
    pub(super) config: Option<PathBuf>,
    /// Link states the console set for saved peers, by peer name. A peer
    /// the console never named is connected.
    links: BTreeMap<String, FlowLinkState>,
    open: bool,
    /// Counts the times pairing opened, so a discovery scheduled before a
    /// close does not reach the next opening.
    opened: u64,
    nearby: Vec<FlowPairingPeer>,
    attempt: Option<Attempt>,
    /// The last attempt's result, shown until dismissed.
    result: Option<FlowPairingPhase>,
    next_attempt: u64,
}

#[derive(Debug)]
struct Attempt {
    /// Proves a scheduled step still belongs to this attempt.
    id: u64,
    /// The address the user typed, saved with the peer.
    address: Option<String>,
    stage: Stage,
}

#[derive(Debug)]
enum Stage {
    Connecting {
        peer: Option<FlowPairingPeer>,
    },
    Code {
        peer: FlowPairingPeer,
        confirmed: bool,
        peer_confirmed: bool,
    },
}

impl MockFlow {
    /// What the snapshot reports: the peers saved in the config, each
    /// connected unless the console set another state and all lost while
    /// Flow is off, as the real agent reports a dormant Flow; and where
    /// pairing stands.
    pub(super) fn status(&self) -> FlowStatus {
        let flow = self
            .config_path()
            .ok()
            .and_then(|path| ConfigFile::load_from_path(&path).ok())
            .map(|(config, _)| config.flow)
            .unwrap_or_default();
        FlowStatus {
            enabled: flow.enabled,
            peers: flow
                .peers()
                .iter()
                .map(|peer| FlowPeerStatus {
                    name: peer.name.clone(),
                    public_key: peer.public_key,
                    state: if flow.enabled {
                        self.links
                            .get(peer.name.as_str())
                            .copied()
                            .unwrap_or(FlowLinkState::Connected)
                    } else {
                        FlowLinkState::Lost
                    },
                })
                .collect(),
            pairing: self.phase(),
        }
    }

    fn phase(&self) -> Option<FlowPairingPhase> {
        if !self.open {
            return None;
        }
        if let Some(attempt) = &self.attempt {
            return Some(match &attempt.stage {
                Stage::Connecting { peer } => FlowPairingPhase::Connecting { peer: peer.clone() },
                Stage::Code {
                    peer,
                    confirmed: false,
                    ..
                } => FlowPairingPhase::ShowingCode {
                    peer: peer.clone(),
                    code: CODE.to_owned(),
                },
                Stage::Code { peer, .. } => FlowPairingPhase::WaitingForPeer {
                    peer: peer.clone(),
                    code: CODE.to_owned(),
                },
            });
        }
        if let Some(result) = &self.result {
            return Some(result.clone());
        }
        Some(if self.nearby.is_empty() {
            FlowPairingPhase::Searching
        } else {
            FlowPairingPhase::Found(self.nearby.clone())
        })
    }

    fn config_path(&self) -> Result<PathBuf, String> {
        match &self.config {
            Some(path) => Ok(path.clone()),
            None => openlogi_core::paths::config_path().map_err(|error| error.to_string()),
        }
    }

    /// Open pairing, answering which opening the discoveries belong to, or
    /// `None` when it already was open.
    fn open(&mut self) -> Option<u64> {
        if self.open {
            return None;
        }
        self.open = true;
        self.opened += 1;
        Some(self.opened)
    }

    fn discovered(&mut self, opened: u64, index: usize) {
        if self.open && self.opened == opened {
            let (name, key, platform, _) = NEARBY[index];
            self.nearby.push(peer(name, key, platform));
        }
    }

    fn pair(&mut self, target: FlowPairTarget) -> Result<u64, FlowPairingCommandError> {
        if !self.open {
            return Err(FlowPairingCommandError::NotStarted);
        }
        if self.attempt.is_some() {
            return Err(FlowPairingCommandError::Busy);
        }
        let (peer, address) = match target {
            FlowPairTarget::Nearby(key) => {
                let peer = self
                    .nearby
                    .iter()
                    .find(|peer| peer.public_key == key)
                    .cloned()
                    .ok_or(FlowPairingCommandError::UnknownPeer)?;
                (Some(peer), None)
            }
            FlowPairTarget::Address(address) => (None, Some(address)),
        };
        Ok(self.begin(address, Stage::Connecting { peer }))
    }

    fn begin(&mut self, address: Option<String>, stage: Stage) -> u64 {
        self.result = None;
        self.next_attempt += 1;
        self.attempt = Some(Attempt {
            id: self.next_attempt,
            address,
            stage,
        });
        self.next_attempt
    }

    fn connected(&mut self, id: u64) {
        let Some(attempt) = self.attempt.as_mut().filter(|attempt| attempt.id == id) else {
            return;
        };
        if let Stage::Connecting { peer } = &mut attempt.stage {
            let (name, key, platform) = ADDRESS_PEER;
            let peer = peer
                .take()
                .unwrap_or_else(|| self::peer(name, key, platform));
            attempt.stage = Stage::Code {
                peer,
                confirmed: false,
                peer_confirmed: false,
            };
        }
    }

    /// Answers the attempt whose other user still has to confirm.
    fn confirm(&mut self) -> Result<Option<u64>, FlowPairingCommandError> {
        if !self.open {
            return Err(FlowPairingCommandError::NotStarted);
        }
        let Some(Attempt {
            id,
            stage:
                Stage::Code {
                    confirmed: confirmed @ false,
                    peer_confirmed,
                    ..
                },
            ..
        }) = &mut self.attempt
        else {
            return Err(FlowPairingCommandError::NothingToConfirm);
        };
        *confirmed = true;
        if *peer_confirmed {
            self.save();
            Ok(None)
        } else {
            Ok(Some(*id))
        }
    }

    /// The other user confirmed attempt `id`. Answers whether it was still
    /// waiting for that.
    fn peer_confirmed(&mut self, id: u64) -> bool {
        let Some(Attempt {
            stage:
                Stage::Code {
                    confirmed,
                    peer_confirmed: peer_confirmed @ false,
                    ..
                },
            ..
        }) = self.attempt.as_mut().filter(|attempt| attempt.id == id)
        else {
            return false;
        };
        *peer_confirmed = true;
        if *confirmed {
            self.save();
        }
        true
    }

    /// Both users confirmed: save the peer as the agent does.
    fn save(&mut self) {
        let Some(Attempt {
            address,
            stage: Stage::Code { peer, .. },
            ..
        }) = self.attempt.take()
        else {
            return;
        };
        let paired = PairedPeer {
            public_key: peer.public_key,
            name: &peer.name,
            address: address.as_deref(),
        };
        let saved = self
            .config_path()
            .and_then(|path| save_peer(&path, &paired).map_err(|error| error.to_string()));
        self.result = Some(match saved {
            Ok(name) => FlowPairingPhase::Paired { peer, name },
            Err(message) => FlowPairingPhase::Failed {
                peer: Some(peer),
                reason: FlowPairingFailure::SaveFailed { message },
            },
        });
    }

    fn cancel(&mut self) -> Result<(), FlowPairingCommandError> {
        if !self.open {
            return Err(FlowPairingCommandError::NotStarted);
        }
        self.attempt = None;
        self.result = None;
        Ok(())
    }

    fn stop(&mut self) {
        self.open = false;
        self.nearby.clear();
        self.attempt = None;
        self.result = None;
    }

    /// Apply a console `flow` command; answers what to tell the developer.
    fn apply(&mut self, command: FlowCommand) -> Result<String, String> {
        match command {
            FlowCommand::Link { peer, state } => {
                let reply = format!("Flow peer {peer} reports {state:?}");
                self.links.insert(peer, state);
                Ok(reply)
            }
            _ if !self.open => Err("Flow pairing is closed; open it in the GUI first".to_owned()),
            FlowCommand::Incoming => {
                if self.attempt.is_some() {
                    return Err("already pairing; a third computer is turned away".to_owned());
                }
                let (name, key, platform, _) = NEARBY[0];
                self.begin(
                    None,
                    Stage::Code {
                        peer: peer(name, key, platform),
                        confirmed: false,
                        peer_confirmed: false,
                    },
                );
                Ok(format!("{name} is pairing with this computer; code {CODE}"))
            }
            FlowCommand::PeerConfirms => {
                let id = self.attempt.as_ref().map_or(0, |attempt| attempt.id);
                if self.peer_confirmed(id) {
                    Ok("the other computer's user confirmed".to_owned())
                } else {
                    Err("no code is waiting for the other computer's user".to_owned())
                }
            }
            FlowCommand::Fail(reason) => {
                let attempt = self
                    .attempt
                    .take()
                    .ok_or("no pairing attempt to fail; pair with a computer first")?;
                let peer = match attempt.stage {
                    Stage::Connecting { peer } => peer,
                    Stage::Code { peer, .. } => Some(peer),
                };
                let reply = format!("pairing failed: {reason:?}");
                self.result = Some(FlowPairingPhase::Failed { peer, reason });
                Ok(reply)
            }
        }
    }
}

fn peer(name: &str, key: u8, platform: FlowPlatform) -> FlowPairingPeer {
    FlowPairingPeer {
        name: name.to_owned(),
        public_key: FlowPublicKey::from_bytes([key; 32]),
        platform,
    }
}

/// Parse the words after `flow` on a console line.
pub(super) fn parse<'a>(words: &mut impl Iterator<Item = &'a str>) -> Result<FlowCommand, String> {
    match words.next() {
        Some("link") => parse_link(words),
        Some("incoming") => Ok(FlowCommand::Incoming),
        Some("confirm") => Ok(FlowCommand::PeerConfirms),
        Some("fail") => {
            let reason = match words.next() {
                Some("cancel") => FlowPairingFailure::PeerCancelled,
                Some("mismatch") => FlowPairingFailure::CodeMismatch,
                Some("timeout") => FlowPairingFailure::TimedOut,
                Some("unreachable") => FlowPairingFailure::Unreachable,
                Some("busy") => FlowPairingFailure::PeerBusy,
                Some("version") => FlowPairingFailure::IncompatibleVersion,
                Some("lost") => FlowPairingFailure::ConnectionLost,
                Some("save") => FlowPairingFailure::SaveFailed {
                    message: "the mock console made saving fail".to_owned(),
                },
                _ => return Err(format!("flow fail needs one of: {FAILURES}")),
            };
            Ok(FlowCommand::Fail(reason))
        }
        _ => Err(
            "flow needs `link <peer> <state>`, `incoming`, `confirm`, or `fail <reason>`"
                .to_owned(),
        ),
    }
}

/// `flow link <peer> <state>`: a peer name may have spaces, so the state is
/// the last word and the name is every word before it.
fn parse_link<'a>(words: &mut impl Iterator<Item = &'a str>) -> Result<FlowCommand, String> {
    let mut rest: Vec<&str> = words.collect();
    let state = match rest.pop() {
        Some("connected") => FlowLinkState::Connected,
        Some("unstable") => FlowLinkState::Degraded,
        Some("lost") => FlowLinkState::Lost,
        _ => {
            return Err("flow link needs a peer name and connected, unstable or lost".to_owned());
        }
    };
    if rest.is_empty() {
        return Err("flow link needs a peer name before the state".to_owned());
    }
    Ok(FlowCommand::Link {
        peer: rest.join(" "),
        state,
    })
}

/// The reasons `flow fail` takes.
const FAILURES: &str = "cancel mismatch timeout unreachable busy version lost save";

impl MockAgent {
    pub(super) fn flow_identity() -> FlowIdentity {
        FlowIdentity {
            machine_name: MACHINE_NAME.to_owned(),
            public_key: FlowPublicKey::from_bytes(MACHINE_KEY),
        }
    }

    pub(super) async fn open_flow_pairing(&self) -> Result<(), FlowPairingCommandError> {
        let opened = {
            let mut state = self.state.lock().await;
            if !state.clock.is_demo() {
                return Err(FlowPairingCommandError::Unavailable {
                    message: "fixture runs keep test time frozen; pairing runs only in the demo"
                        .to_owned(),
                });
            }
            state.flow.open()
        };
        for (index, (.., delay)) in NEARBY.into_iter().enumerate() {
            let state = Arc::clone(&self.state);
            if let Some(opened) = opened {
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    state.lock().await.flow.discovered(opened, index);
                });
            }
        }
        Ok(())
    }

    pub(super) async fn pair_flow(
        &self,
        target: FlowPairTarget,
    ) -> Result<(), FlowPairingCommandError> {
        let id = self.state.lock().await.flow.pair(target)?;
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            tokio::time::sleep(CONNECT_DELAY).await;
            state.lock().await.flow.connected(id);
        });
        Ok(())
    }

    pub(super) async fn confirm_flow(&self) -> Result<(), FlowPairingCommandError> {
        let waiting = self.state.lock().await.flow.confirm()?;
        if let Some(id) = waiting {
            let state = Arc::clone(&self.state);
            tokio::spawn(async move {
                tokio::time::sleep(PEER_CONFIRM_DELAY).await;
                state.lock().await.flow.peer_confirmed(id);
            });
        }
        Ok(())
    }

    pub(super) async fn cancel_flow(&self) -> Result<(), FlowPairingCommandError> {
        self.state.lock().await.flow.cancel()
    }

    pub(super) async fn stop_flow(&self) {
        self.state.lock().await.flow.stop();
    }

    pub(super) async fn apply_flow(&self, command: FlowCommand) -> Result<String, String> {
        self.state.lock().await.flow.apply(command)
    }
}
