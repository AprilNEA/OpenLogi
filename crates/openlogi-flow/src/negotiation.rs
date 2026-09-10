//! Pure protocol-version, identity, and capability negotiation.

use std::collections::BTreeSet;

use buffa::EnumValue;

use crate::{frame::FrameKind, proto::openlogi::flow::v1 as proto, sas::PublicKey};

/// A capability understood by this implementation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Capability {
    /// Text clipboard representations and their bulk transfer frames.
    ClipboardText,
    /// Clipboard file-list representations and individual file fetches.
    ClipboardFiles,
}

/// Successful result of negotiating two `Hello` messages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Negotiated {
    /// Highest mutually supported protocol version.
    pub version: u32,
    /// Known capabilities advertised by both peers.
    pub capabilities: BTreeSet<Capability>,
}

/// A typed reason to reject the peer's `Hello`.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HelloRejection {
    /// The two inclusive version ranges do not overlap.
    #[error("protocol version ranges are disjoint")]
    VersionDisjoint,
    /// The protobuf key does not match the authenticated TLS identity.
    #[error("Hello public key does not match TLS identity")]
    KeyMismatch,
    /// A frozen Hello field violates its required shape.
    #[error("Hello contains an invalid version range or session nonce")]
    Malformed,
}

impl HelloRejection {
    /// Returns the corresponding protobuf rejection reason.
    #[must_use]
    pub const fn reason(&self) -> proto::RejectReason {
        match self {
            Self::VersionDisjoint => proto::RejectReason::VersionDisjoint,
            Self::KeyMismatch => proto::RejectReason::KeyMismatch,
            Self::Malformed => proto::RejectReason::Malformed,
        }
    }
}

/// Negotiates a local and peer `Hello` after TLS authentication.
///
/// Unknown capability numbers are deliberately ignored: protobuf evolution
/// requires accepting them, but this implementation cannot enable semantics
/// it does not understand.
pub fn negotiate(
    local: &proto::Hello,
    peer: &proto::Hello,
    peer_tls_identity: &PublicKey,
) -> Result<Negotiated, HelloRejection> {
    let hello_key =
        PublicKey::try_from(peer.public_key.as_slice()).map_err(|_| HelloRejection::KeyMismatch)?;
    if &hello_key != peer_tls_identity {
        return Err(HelloRejection::KeyMismatch);
    }
    if peer.proto_min == 0
        || peer.proto_min > peer.proto_max
        || peer.session_nonce.len() != 16
        || local.proto_min == 0
        || local.proto_min > local.proto_max
        || local.session_nonce.len() != 16
    {
        return Err(HelloRejection::Malformed);
    }

    let version = local.proto_max.min(peer.proto_max);
    if version < local.proto_min.max(peer.proto_min) {
        return Err(HelloRejection::VersionDisjoint);
    }

    let local_caps = known_capabilities(&local.capabilities);
    let peer_caps = known_capabilities(&peer.capabilities);
    Ok(Negotiated {
        version,
        capabilities: local_caps.intersection(&peer_caps).copied().collect(),
    })
}

/// Send-side protocol and capability gate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SendGate {
    negotiated: Negotiated,
}

impl SendGate {
    /// Creates a gate for a completed negotiation.
    #[must_use]
    pub const fn new(negotiated: Negotiated) -> Self {
        Self { negotiated }
    }

    /// Returns whether the peer enabled a known capability.
    #[must_use]
    pub fn may_send_capability(&self, capability: Capability) -> bool {
        self.negotiated.capabilities.contains(&capability)
    }

    /// Returns whether a frame kind may be sent.
    ///
    /// All current kinds are version 1. Link and pairing kinds are always
    /// legal at v1. Clipboard announce/fetch/data use `CLIPBOARD_TEXT`;
    /// `FileFetch` uses `CLIPBOARD_FILES`. `Chunk` and `ChunkEnd` are legal
    /// when either clipboard capability is enabled because they carry both
    /// text and file-fetch response bodies.
    #[must_use]
    pub fn may_send_kind(&self, kind: FrameKind) -> bool {
        if self.negotiated.version < 1 || !kind.is_supported() {
            return false;
        }
        match kind {
            FrameKind::ClipboardAnnounce | FrameKind::ClipboardFetch => {
                self.may_send_capability(Capability::ClipboardText)
            }
            FrameKind::FileFetch => self.may_send_capability(Capability::ClipboardFiles),
            FrameKind::ClipboardData | FrameKind::Chunk | FrameKind::ChunkEnd => {
                self.may_send_capability(Capability::ClipboardText)
                    || self.may_send_capability(Capability::ClipboardFiles)
            }
            _ => true,
        }
    }
}

fn known_capabilities(values: &[EnumValue<proto::Capability>]) -> BTreeSet<Capability> {
    values
        .iter()
        .filter_map(|value| match value.to_i32() {
            1 => Some(Capability::ClipboardText),
            2 => Some(Capability::ClipboardFiles),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(min: u32, max: u32, key: [u8; 32], caps: &[i32]) -> proto::Hello {
        proto::Hello {
            proto_min: min,
            proto_max: max,
            public_key: key.to_vec(),
            session_nonce: vec![0; 16],
            capabilities: caps.iter().copied().map(EnumValue::from).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn overlap_uses_lower_max_and_intersects_known_caps() {
        let key = PublicKey::from([7; 32]);
        let local = hello(1, 3, [1; 32], &[1, 2, 99]);
        let peer = hello(2, 4, [7; 32], &[1, 99]);
        let result = negotiate(&local, &peer, &key).unwrap();
        assert_eq!(result.version, 3);
        assert_eq!(
            result.capabilities,
            BTreeSet::from([Capability::ClipboardText])
        );
    }

    #[test]
    fn disjoint_versions_are_rejected() {
        let key = PublicKey::from([7; 32]);
        assert_eq!(
            negotiate(&hello(1, 1, [1; 32], &[]), &hello(2, 3, [7; 32], &[]), &key),
            Err(HelloRejection::VersionDisjoint)
        );
    }

    #[test]
    fn key_mismatch_is_rejected() {
        let key = PublicKey::from([8; 32]);
        assert_eq!(
            negotiate(&hello(1, 1, [1; 32], &[]), &hello(1, 1, [7; 32], &[]), &key),
            Err(HelloRejection::KeyMismatch)
        );
    }

    #[test]
    fn malformed_nonce_is_rejected() {
        let key = PublicKey::from([7; 32]);
        let local = hello(1, 1, [1; 32], &[]);
        let mut peer = hello(1, 1, [7; 32], &[]);
        peer.session_nonce = vec![0; 15];
        assert_eq!(
            negotiate(&local, &peer, &key),
            Err(HelloRejection::Malformed)
        );
    }

    #[test]
    fn send_gate_applies_family_capabilities() {
        let gate = SendGate::new(Negotiated {
            version: 1,
            capabilities: BTreeSet::from([Capability::ClipboardFiles]),
        });
        assert!(gate.may_send_kind(FrameKind::PairStart));
        assert!(!gate.may_send_kind(FrameKind::ClipboardFetch));
        assert!(gate.may_send_kind(FrameKind::FileFetch));
        assert!(gate.may_send_kind(FrameKind::ClipboardData));
        assert!(gate.may_send_kind(FrameKind::Chunk));
        assert!(!gate.may_send_kind(FrameKind::Unknown(0x99)));
    }
}
