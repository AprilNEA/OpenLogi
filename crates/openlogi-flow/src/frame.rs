//! Frozen Flow frame envelope and protobuf payload helpers.

use std::io;

use buffa::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::proto::openlogi::flow::v1 as proto;

/// Number of bytes in the frozen frame header.
pub const HEADER_LEN: usize = 8;
/// Maximum payload size for ordinary frames.
pub const MAX_PAYLOAD_LEN: usize = 1024 * 1024;
/// Maximum payload size for [`FrameKind::Chunk`].
pub const MAX_CHUNK_LEN: usize = 64 * 1024;

/// A frame discriminator, preserving values introduced by newer peers.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FrameKind {
    /// The invalid zero discriminator.
    Unspecified,
    /// Link negotiation.
    Hello,
    /// Rejected link negotiation.
    HelloReject,
    /// Generic RPC error.
    Error,
    /// Liveness request.
    Ping,
    /// Liveness response.
    Pong,
    /// Pairing request.
    PairStart,
    /// Pairing prompt acknowledgement.
    PairPrompted,
    /// Pairing confirmation.
    PairConfirm,
    /// Pairing outcome.
    PairOutcome,
    /// Pairing cancellation.
    PairAbort,
    /// Peer information request.
    GetPeerInfo,
    /// Peer information response.
    PeerInfo,
    /// Device-state notification.
    AnnounceDevices,
    /// Peer-state notification.
    PeerState,
    /// Device handoff request.
    HandoffRequest,
    /// Accepted handoff.
    HandoffAccept,
    /// Rejected handoff.
    HandoffReject,
    /// Handoff result notification.
    HandoffResult,
    /// Handoff cancellation notification.
    HandoffCancel,
    /// Clipboard availability notification.
    ClipboardAnnounce,
    /// Clipboard payload request.
    ClipboardFetch,
    /// Bulk payload response head.
    ClipboardData,
    /// Bulk payload slice.
    Chunk,
    /// Bulk payload terminator.
    ChunkEnd,
    /// Clipboard file request.
    FileFetch,
    /// A discriminator unknown to this implementation.
    Unknown(u16),
}

impl FrameKind {
    /// Returns the frozen wire value.
    #[must_use]
    pub const fn wire_value(self) -> u16 {
        match self {
            Self::Unspecified => 0x00,
            Self::Hello => 0x01,
            Self::HelloReject => 0x02,
            Self::Error => 0x03,
            Self::Ping => 0x04,
            Self::Pong => 0x05,
            Self::PairStart => 0x10,
            Self::PairPrompted => 0x11,
            Self::PairConfirm => 0x12,
            Self::PairOutcome => 0x13,
            Self::PairAbort => 0x14,
            Self::GetPeerInfo => 0x20,
            Self::PeerInfo => 0x21,
            Self::AnnounceDevices => 0x22,
            Self::PeerState => 0x23,
            Self::HandoffRequest => 0x30,
            Self::HandoffAccept => 0x31,
            Self::HandoffReject => 0x32,
            Self::HandoffResult => 0x33,
            Self::HandoffCancel => 0x34,
            Self::ClipboardAnnounce => 0x40,
            Self::ClipboardFetch => 0x41,
            Self::ClipboardData => 0x42,
            Self::Chunk => 0x43,
            Self::ChunkEnd => 0x44,
            Self::FileFetch => 0x45,
            Self::Unknown(value) => value,
        }
    }

    /// Returns the payload cap for this kind.
    #[must_use]
    pub const fn payload_cap(self) -> usize {
        if matches!(self, Self::Chunk) {
            MAX_CHUNK_LEN
        } else {
            MAX_PAYLOAD_LEN
        }
    }

    /// Returns whether the discriminator is known and meaningful.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        !matches!(self, Self::Unspecified | Self::Unknown(_))
    }
}

impl From<u16> for FrameKind {
    fn from(value: u16) -> Self {
        match value {
            0 => Self::Unspecified,
            1 => Self::Hello,
            2 => Self::HelloReject,
            3 => Self::Error,
            4 => Self::Ping,
            5 => Self::Pong,
            0x10 => Self::PairStart,
            0x11 => Self::PairPrompted,
            0x12 => Self::PairConfirm,
            0x13 => Self::PairOutcome,
            0x14 => Self::PairAbort,
            0x20 => Self::GetPeerInfo,
            0x21 => Self::PeerInfo,
            0x22 => Self::AnnounceDevices,
            0x23 => Self::PeerState,
            0x30 => Self::HandoffRequest,
            0x31 => Self::HandoffAccept,
            0x32 => Self::HandoffReject,
            0x33 => Self::HandoffResult,
            0x34 => Self::HandoffCancel,
            0x40 => Self::ClipboardAnnounce,
            0x41 => Self::ClipboardFetch,
            0x42 => Self::ClipboardData,
            0x43 => Self::Chunk,
            0x44 => Self::ChunkEnd,
            0x45 => Self::FileFetch,
            other => Self::Unknown(other),
        }
    }
}

impl From<proto::FrameKind> for FrameKind {
    fn from(value: proto::FrameKind) -> Self {
        Self::from(value as u16)
    }
}

impl TryFrom<FrameKind> for proto::FrameKind {
    type Error = u16;
    fn try_from(value: FrameKind) -> Result<Self, Self::Error> {
        use buffa::Enumeration;
        let wire = value.wire_value();
        proto::FrameKind::from_i32(i32::from(wire)).ok_or(wire)
    }
}

/// An owned, validated envelope and its opaque payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Envelope {
    /// Frame discriminator.
    pub kind: FrameKind,
    /// Reserved envelope flags; senders must use zero.
    pub flags: u16,
    /// Protobuf payload bytes.
    pub payload: Vec<u8>,
}

/// A frame paired with its decoded protobuf message.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame<M> {
    /// Frame discriminator.
    pub kind: FrameKind,
    /// Reserved envelope flags.
    pub flags: u16,
    /// Decoded protobuf payload.
    pub message: M,
}

/// Why an envelope could not be read or encoded.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The input ended before the declared frame ended.
    #[error("truncated frame")]
    Truncated,
    /// The declared payload exceeds its kind-specific cap.
    #[error("frame payload length {len} exceeds cap {cap}")]
    TooLarge {
        /// Declared payload length.
        len: usize,
        /// Applicable limit.
        cap: usize,
    },
    /// Protobuf encoding exceeded buffa's limit.
    #[error("protobuf encoding failed: {0}")]
    Encode(#[from] buffa::EncodeError),
    /// An asynchronous transport operation failed.
    #[error("frame transport failed: {0}")]
    Io(#[from] io::Error),
}

impl FrameError {
    /// Returns the protocol action when this error represents an oversized frame.
    #[must_use]
    pub const fn too_large_decision(&self, role: InboundRole) -> Option<InvalidDecision> {
        match self {
            Self::TooLarge { .. } => Some(role.invalid(proto::ErrorCode::TooLarge)),
            _ => None,
        }
    }
}

/// Protocol action for a malformed, unsupported, or oversized inbound frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidDecision {
    /// Silently discard a notification.
    Drop,
    /// Send an `Error` response with this code for a request.
    Respond(proto::ErrorCode),
}

/// Whether an inbound frame occupies a response-bearing request position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboundRole {
    /// A one-way notification.
    Notification,
    /// A request on a bidirectional stream.
    Request,
}

impl InboundRole {
    /// Chooses the spec-mandated action for an error code.
    #[must_use]
    pub const fn invalid(self, code: proto::ErrorCode) -> InvalidDecision {
        match self {
            Self::Notification => InvalidDecision::Drop,
            Self::Request => InvalidDecision::Respond(code),
        }
    }
}

impl Envelope {
    /// Creates an envelope, enforcing the kind-specific payload cap.
    pub fn new(kind: FrameKind, flags: u16, payload: Vec<u8>) -> Result<Self, FrameError> {
        check_len(kind, payload.len())?;
        Ok(Self {
            kind,
            flags,
            payload,
        })
    }

    /// Encodes one complete envelope into bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, FrameError> {
        check_len(self.kind, self.payload.len())?;
        let len = u32::try_from(self.payload.len()).map_err(|_| FrameError::TooLarge {
            len: self.payload.len(),
            cap: self.kind.payload_cap(),
        })?;
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.extend_from_slice(&self.kind.wire_value().to_le_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    /// Decodes exactly one envelope from a byte slice.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, FrameError> {
        if bytes.len() < HEADER_LEN {
            return Err(FrameError::Truncated);
        }
        let kind = FrameKind::from(u16::from_le_bytes([bytes[0], bytes[1]]));
        let flags = u16::from_le_bytes([bytes[2], bytes[3]]);
        let len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        check_len(kind, len)?;
        if bytes.len() != HEADER_LEN + len {
            return Err(FrameError::Truncated);
        }
        Ok(Self {
            kind,
            flags,
            payload: bytes[HEADER_LEN..].to_vec(),
        })
    }

    /// Reads one envelope from a Tokio-compatible asynchronous reader.
    pub async fn read_from(reader: &mut (impl AsyncRead + Unpin)) -> Result<Self, FrameError> {
        let mut header = [0; HEADER_LEN];
        reader.read_exact(&mut header).await?;
        let kind = FrameKind::from(u16::from_le_bytes([header[0], header[1]]));
        let flags = u16::from_le_bytes([header[2], header[3]]);
        let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        check_len(kind, len)?;
        let mut payload = vec![0; len];
        reader.read_exact(&mut payload).await?;
        Ok(Self {
            kind,
            flags,
            payload,
        })
    }

    /// Writes one envelope to a Tokio-compatible asynchronous writer.
    pub async fn write_to(&self, writer: &mut (impl AsyncWrite + Unpin)) -> Result<(), FrameError> {
        writer.write_all(&self.to_bytes()?).await?;
        Ok(())
    }

    /// Applies unknown-kind and unknown-flags policy before payload decoding.
    #[must_use]
    pub const fn policy(&self, role: InboundRole) -> Option<InvalidDecision> {
        if !self.kind.is_supported() {
            Some(role.invalid(proto::ErrorCode::UnsupportedKind))
        } else if self.flags != 0 {
            Some(role.invalid(proto::ErrorCode::UnsupportedFlags))
        } else {
            None
        }
    }

    /// Decodes the payload, returning the spec's `INVALID` action on failure.
    pub fn decode<M: Message>(&self, role: InboundRole) -> Result<M, InvalidDecision> {
        M::decode_from_slice(&self.payload).map_err(|_| role.invalid(proto::ErrorCode::Invalid))
    }
}

impl<M: Message> Frame<M> {
    /// Encodes a typed protobuf frame while enforcing envelope limits.
    pub fn encode(kind: FrameKind, message: M) -> Result<Self, FrameError> {
        let mut payload = Vec::new();
        message.try_encode(&mut payload)?;
        check_len(kind, payload.len())?;
        Ok(Self {
            kind,
            flags: 0,
            message,
        })
    }

    /// Converts this typed frame to an opaque envelope.
    pub fn into_envelope(self) -> Result<Envelope, FrameError> {
        let mut payload = Vec::new();
        self.message.try_encode(&mut payload)?;
        Envelope::new(self.kind, self.flags, payload)
    }
}

fn check_len(kind: FrameKind, len: usize) -> Result<(), FrameError> {
    let cap = kind.payload_cap();
    if len > cap {
        Err(FrameError::TooLarge { len, cap })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_bytes_freeze_layout() {
        let frame = Envelope::new(FrameKind::Hello, 0x0201, vec![0xaa, 0xbb, 0xcc]).unwrap();
        assert_eq!(
            frame.to_bytes().unwrap(),
            [0x01, 0x00, 0x01, 0x02, 0x03, 0, 0, 0, 0xaa, 0xbb, 0xcc]
        );
    }

    #[test]
    fn unknown_kind_is_preserved_and_policy_depends_on_role() {
        let frame = Envelope::from_bytes(&[0xfe, 0xca, 0, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(frame.kind, FrameKind::Unknown(0xcafe));
        assert_eq!(
            frame.policy(InboundRole::Notification),
            Some(InvalidDecision::Drop)
        );
        assert_eq!(
            frame.policy(InboundRole::Request),
            Some(InvalidDecision::Respond(proto::ErrorCode::UnsupportedKind))
        );
    }

    #[test]
    fn flags_and_decode_failure_have_typed_decisions() {
        let flagged = Envelope::new(FrameKind::Ping, 1, Vec::new()).unwrap();
        assert_eq!(
            flagged.policy(InboundRole::Request),
            Some(InvalidDecision::Respond(proto::ErrorCode::UnsupportedFlags))
        );
        let invalid = Envelope::new(FrameKind::Ping, 0, vec![0xff]).unwrap();
        assert_eq!(
            invalid.decode::<proto::Ping>(InboundRole::Notification),
            Err(InvalidDecision::Drop)
        );
    }

    #[test]
    fn chunk_has_smaller_cap() {
        let error = Envelope::new(FrameKind::Chunk, 0, vec![0; MAX_CHUNK_LEN + 1]).unwrap_err();
        assert!(matches!(
            error,
            FrameError::TooLarge {
                cap: MAX_CHUNK_LEN,
                ..
            }
        ));
        Envelope::new(FrameKind::Ping, 0, vec![0; MAX_CHUNK_LEN + 1]).unwrap();
    }

    #[test]
    fn truncated_and_declared_oversize_are_rejected() {
        assert!(matches!(
            Envelope::from_bytes(&[1, 0]),
            Err(FrameError::Truncated)
        ));
        let mut header = [0; HEADER_LEN];
        header[0] = 0x43;
        header[4..].copy_from_slice(&(u32::try_from(MAX_CHUNK_LEN).unwrap() + 1).to_le_bytes());
        assert!(matches!(
            Envelope::from_bytes(&header),
            Err(FrameError::TooLarge { .. })
        ));
    }
}
