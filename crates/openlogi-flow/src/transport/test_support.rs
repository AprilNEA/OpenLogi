//! Crate-internal helpers for tests that need a misbehaving raw QUIC peer.

use std::net::{Ipv4Addr, SocketAddr};

use quinn::{Connection, Endpoint};

use super::{MachineIdentity, PeerTrust, quic_configs};
use crate::generated as proto;

/// Binds to an ephemeral loopback port.
pub(crate) fn localhost() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
}

/// A valid `Hello` template for `identity`; the endpoint fills the nonce.
pub(crate) fn hello(identity: &MachineIdentity) -> proto::Hello {
    proto::Hello {
        proto_min: 1,
        proto_max: 1,
        public_key: identity.public_key().as_bytes().to_vec(),
        machine_name: "transport-test".to_owned(),
        app_version: "test".to_owned(),
        ..Default::default()
    }
}

/// A peer that finished TLS and then went silent.
pub(crate) struct StalledDial {
    _endpoint: Endpoint,
    _connection: Connection,
}

/// Completes TLS against `address` and never opens the control stream.
pub(crate) async fn stalled_dial(
    identity: &MachineIdentity,
    trust: PeerTrust,
    address: SocketAddr,
) -> StalledDial {
    let (_, client_config) = quic_configs(identity, trust).unwrap();
    let endpoint = Endpoint::client(localhost()).unwrap();
    let connection = endpoint
        .connect_with(client_config, address, "openlogi-flow")
        .unwrap()
        .await
        .unwrap();
    StalledDial {
        _endpoint: endpoint,
        _connection: connection,
    }
}
