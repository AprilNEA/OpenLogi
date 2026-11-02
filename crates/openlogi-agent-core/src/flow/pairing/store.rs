//! Saving a paired computer as a `[[flow.peers]]` entry.
//!
//! The agent writes `config.toml` here and nowhere else, through the same
//! [`ConfigFile::save`] as the GUI and `openlogi api`: it refuses to overwrite
//! a file that changed since it was read, so a conflict reads the file again
//! and merges the peer into it.

use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use openlogi_core::config::{
    ConfigError, ConfigFile, FlowConfig, FlowConfigError, FlowPeer, FlowPeerName, FlowPublicKey,
};
use openlogi_flow::pairing::{PeerKeyStore, PersistPeerKeyError};
use openlogi_flow::sas::PublicKey;
use thiserror::Error;

/// How many times a save reads the file again after another writer changed
/// it or held its lock.
const SAVE_RETRIES: u32 = 5;
/// How long a save waits for another writer to release the config lock.
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(20);
/// The name a computer that names itself nothing is saved under.
const UNNAMED_PEER: &str = "Computer";
/// The longest saved peer name, in characters.
const MAX_PEER_NAME: usize = 64;

/// A computer to save as a Flow peer.
#[derive(Clone, Copy, Debug)]
pub struct PairedPeer<'a> {
    /// The key the pairing ceremony authenticated.
    pub public_key: FlowPublicKey,
    /// The name the computer gave itself. It is made a valid, unused peer
    /// name before saving.
    pub name: &'a str,
    /// The address the user typed to pair by, if any. Its host is saved for
    /// the Flow runtime to dial when mDNS cannot find the computer; its port,
    /// a pairing port, is not.
    pub address: Option<&'a str>,
}

/// Why a paired computer could not be saved.
#[derive(Debug, Error)]
pub enum SavePeerError {
    /// Reading or writing `config.toml` failed.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The `[flow]` section with the peer added does not validate.
    #[error(transparent)]
    Flow(#[from] FlowConfigError),
}

/// Save `peer` in the `[[flow.peers]]` of the config at `path`, with no
/// screen edge, and answer the name it is saved under.
///
/// A key already saved keeps its entry and name, and gains `peer.address`.
/// The file is rewritten only when something changed.
pub fn save_peer(path: &Path, peer: &PairedPeer<'_>) -> Result<FlowPeerName, SavePeerError> {
    let mut retries = 0;
    loop {
        let (mut config, mut file) = ConfigFile::load_from_path(path)?;
        let (flow, name) = with_peer(config.flow.clone(), peer)?;
        let Some(flow) = flow else {
            return Ok(name);
        };
        config.flow = flow;
        match file.save(&config) {
            Ok(()) => return Ok(name),
            Err(ConfigError::Conflict { .. }) if retries < SAVE_RETRIES => {}
            Err(ConfigError::Write { source, .. })
                if source.kind() == io::ErrorKind::WouldBlock && retries < SAVE_RETRIES =>
            {
                std::thread::sleep(LOCK_RETRY_DELAY);
            }
            Err(error) => return Err(error.into()),
        }
        retries += 1;
    }
}

/// `flow` with `peer` saved in it, or `None` when `flow` already holds
/// everything `peer` would add, and the name `peer` has there.
fn with_peer(
    flow: FlowConfig,
    peer: &PairedPeer<'_>,
) -> Result<(Option<FlowConfig>, FlowPeerName), FlowConfigError> {
    let mut parts = flow.into_parts();
    let address = peer.address.and_then(address_hint);
    let saved = parts
        .peers
        .iter_mut()
        .find(|saved| saved.public_key == peer.public_key);
    let name = if let Some(saved) = saved {
        match address {
            Some(address) if !saved.addresses.contains(&address) => saved.addresses.push(address),
            _ => return Ok((None, saved.name.clone())),
        }
        saved.name.clone()
    } else {
        let name = unused_name(peer.name, &parts.peers);
        parts.peers.push(FlowPeer {
            name: name.clone(),
            public_key: peer.public_key,
            addresses: address.into_iter().collect(),
        });
        name
    };
    Ok((Some(FlowConfig::try_from(parts)?), name))
}

/// `wanted` as a valid peer name that no saved peer uses: without control
/// characters or surrounding space, at most [`MAX_PEER_NAME`] characters, and
/// numbered ` (2)`, ` (3)`, … when it is taken.
fn unused_name(wanted: &str, saved: &[FlowPeer]) -> FlowPeerName {
    let cleaned: String = wanted
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_PEER_NAME)
        .collect();
    let base = match cleaned.trim() {
        "" => UNNAMED_PEER,
        trimmed => trimmed,
    };
    let mut number = 1_u32;
    loop {
        let candidate = if number == 1 {
            base.to_owned()
        } else {
            format!("{base} ({number})")
        };
        if let Ok(name) = FlowPeerName::new(candidate)
            && !saved.iter().any(|peer| peer.name == name)
        {
            return name;
        }
        number += 1;
    }
}

/// The host of an address the user typed to pair by, saved as the peer's
/// address: the Flow runtime dials it on its own port, not the pairing one.
fn address_hint(address: &str) -> Option<String> {
    let address = address.trim();
    if let Ok(socket) = address.parse::<SocketAddr>() {
        return Some(match socket {
            // A link-local address reaches the peer only through its interface.
            SocketAddr::V6(socket) if socket.scope_id() != 0 => {
                format!("[{}%{}]", socket.ip(), socket.scope_id())
            }
            socket => socket.ip().to_string(),
        });
    }
    let host = match address.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') && port.parse::<u16>().is_ok() => host,
        _ => address,
    };
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .filter(|host| !host.contains('%'))
        .unwrap_or(host);
    (!host.is_empty()).then(|| host.to_owned())
}

/// The ceremony's [`PeerKeyStore`]: saves the peer in `config.toml` when
/// both users confirmed. Saving blocks on the disk, so the session runs the
/// steps that use this store on a blocking thread.
pub(super) struct ConfigPeerStore {
    /// `None` saves to the user's config file.
    pub(super) path: Option<PathBuf>,
    pub(super) name: String,
    pub(super) address: Option<String>,
    /// The name the peer was saved under, once it was.
    pub(super) saved: Option<FlowPeerName>,
}

impl PeerKeyStore for ConfigPeerStore {
    fn persist_peer_key(&mut self, key: PublicKey) -> Result<(), PersistPeerKeyError> {
        let path = match &self.path {
            Some(path) => path.clone(),
            None => user_config_path()?,
        };
        let peer = PairedPeer {
            public_key: FlowPublicKey::from_bytes(*key.as_bytes()),
            name: &self.name,
            address: self.address.as_deref(),
        };
        let name =
            save_peer(&path, &peer).map_err(|error| PersistPeerKeyError::new(error.to_string()))?;
        self.saved = Some(name);
        Ok(())
    }
}

fn user_config_path() -> Result<PathBuf, PersistPeerKeyError> {
    openlogi_core::paths::config_path().map_err(|error| PersistPeerKeyError::new(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use openlogi_core::config::{Config, FlowPublicKey};

    use super::{PairedPeer, address_hint, save_peer};

    fn peer<'a>(byte: u8, name: &'a str, address: Option<&'a str>) -> PairedPeer<'a> {
        PairedPeer {
            public_key: FlowPublicKey::from_bytes([byte; 32]),
            name,
            address,
        }
    }

    /// A new computer gets an entry and no edge; a second one with the same
    /// name is numbered; pairing a saved key again keeps its entry and adds
    /// the new address; everything else in the file stays.
    #[test]
    fn paired_computers_are_merged_into_the_existing_config() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        let mut seed = Config::default();
        seed.app_settings.language = Some("de".to_owned());
        seed.save_to_path(&path).expect("seed the config");
        let seeded = fs::read_to_string(&path).expect("read the config");
        fs::write(&path, format!("# keep me\n{seeded}")).expect("write the config");

        let first = save_peer(&path, &peer(1, " iMac\u{7}", None)).expect("save");
        assert_eq!(first.as_str(), "iMac");
        let second = save_peer(&path, &peer(2, "iMac", Some("10.0.0.7:42425"))).expect("save");
        assert_eq!(second.as_str(), "iMac (2)");
        let again = save_peer(&path, &peer(1, "Renamed", Some("imac.lan"))).expect("save");
        assert_eq!(again.as_str(), "iMac");
        let unnamed = save_peer(&path, &peer(3, "self", None)).expect("save");
        assert_eq!(unnamed.as_str(), "self (2)");

        let text = fs::read_to_string(&path).expect("read the config");
        assert!(text.contains("# keep me"), "{text}");
        let config = Config::load_from_path(&path).expect("the saved config loads");
        assert_eq!(config.app_settings.language.as_deref(), Some("de"));
        assert!(config.flow.layout().is_empty());
        let peers: Vec<_> = config
            .flow
            .peers()
            .iter()
            .map(|peer| {
                (
                    peer.name.as_str(),
                    peer.public_key.as_bytes()[0],
                    peer.addresses.clone(),
                )
            })
            .collect();
        assert_eq!(
            peers,
            [
                ("iMac", 1, vec!["imac.lan".to_owned()]),
                ("iMac (2)", 2, vec!["10.0.0.7".to_owned()]),
                ("self (2)", 3, Vec::new()),
            ]
        );
    }

    #[test]
    fn typed_addresses_are_saved_as_hosts() {
        let cases = [
            ("10.0.0.7", Some("10.0.0.7")),
            ("10.0.0.7:42425", Some("10.0.0.7")),
            ("devbox.tailnet.ts.net", Some("devbox.tailnet.ts.net")),
            (" devbox:5000 ", Some("devbox")),
            ("[fd00::7]:42425", Some("fd00::7")),
            ("[fd00::7]", Some("fd00::7")),
            ("fd00::7", Some("fd00::7")),
            ("[fe80::1234%3]:42425", Some("[fe80::1234%3]")),
            ("[fe80::1234%3]", Some("[fe80::1234%3]")),
            ("", None),
        ];
        for (typed, host) in cases {
            assert_eq!(address_hint(typed).as_deref(), host, "{typed}");
        }
    }
}
