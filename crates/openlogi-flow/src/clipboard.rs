//! Text transfer over the capability-gated clipboard family.
//!
//! The holder of a clipboard generation announces it ([`TextOffer::announce`]);
//! a peer that wants the text fetches it ([`fetch_text`]), and the holder
//! answers from whatever generation it offers at that moment
//! ([`serve_fetch`]). The bytes ride the fetch's own stream as
//! `ClipboardData`, `Chunk`*, `ChunkEnd` (FLOW-PROTOCOL.md §Clipboard & files).
//! Text is UTF-8 under [`TEXT_MIME`], bounded by [`MAX_TEXT_BYTES`] on both
//! ends, and none of these types print it: their `Debug` output names sizes
//! and sequences only.

use std::fmt;

use buffa::EnumValue;
use sha2::{Digest as _, Sha256};

use crate::{
    frame::{Envelope, FrameKind, InboundRole, MAX_CHUNK_LEN},
    generated as proto,
    negotiation::Capability,
    transport::{
        BulkRpcResponse, FlowConnection, IncomingRpc, TransportError, error_envelope,
        message_envelope,
    },
};

/// Media type of the UTF-8 text representation.
pub const TEXT_MIME: &str = "text/plain;charset=utf-8";

/// The most UTF-8 bytes of text offered or accepted. A larger clipboard is
/// skipped whole, never truncated.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;

/// Text bytes per `Chunk`: the frame cap minus the protobuf key (1 byte) and
/// length prefix (3 bytes below 2^21) that frame `Chunk.data`.
const CHUNK_TEXT_LEN: usize = MAX_CHUNK_LEN - 4;

/// Whether `connection` negotiated text clipboard sync: both peers listed
/// `CAPABILITY_CLIPBOARD_TEXT` in their `Hello`. Without it no clipboard frame
/// may be sent either way.
#[must_use]
pub fn text_sync_enabled(connection: &FlowConnection) -> bool {
    connection
        .negotiated()
        .capabilities
        .contains(&Capability::ClipboardText)
}

/// SHA-256 of a text's UTF-8 bytes.
///
/// Announces and transfers carry it so a receiver can verify what it
/// assembled and skip text it already holds. `Debug` redacts the bytes: the
/// digest of a short secret is as good as the secret.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct TextDigest([u8; 32]);

impl TextDigest {
    /// Digests `text`.
    #[must_use]
    pub fn of(text: &str) -> Self {
        Self::of_bytes(text.as_bytes())
    }

    fn of_bytes(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    fn from_wire(bytes: &[u8]) -> Option<Self> {
        <[u8; 32]>::try_from(bytes).ok().map(Self)
    }
}

impl fmt::Debug for TextDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TextDigest(..)")
    }
}

/// Why clipboard text cannot be offered.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OfferError {
    /// There is no text to place on a peer's clipboard.
    #[error("clipboard text is empty")]
    Empty,
    /// The text exceeds [`MAX_TEXT_BYTES`].
    #[error("clipboard text exceeds the {MAX_TEXT_BYTES}-byte sync limit")]
    TooLarge,
}

/// One clipboard generation's text, as this machine offers it to peers.
pub struct TextOffer {
    sequence: u64,
    text: String,
    digest: TextDigest,
}

impl TextOffer {
    /// Wraps the text of clipboard generation `sequence`.
    ///
    /// Sequences are monotonic per sender and start at 1: a receiver drops an
    /// announce that is not newer than the last one it took from the same
    /// connection.
    pub fn new(sequence: u64, text: String) -> Result<Self, OfferError> {
        if text.is_empty() {
            return Err(OfferError::Empty);
        }
        if text.len() > MAX_TEXT_BYTES {
            return Err(OfferError::TooLarge);
        }
        let digest = TextDigest::of(&text);
        Ok(Self {
            sequence,
            text,
            digest,
        })
    }

    /// The clipboard generation this text belongs to.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// The digest of the offered text.
    #[must_use]
    pub const fn digest(&self) -> TextDigest {
        self.digest
    }

    /// The offered text's size in UTF-8 bytes.
    #[must_use]
    pub fn size_bytes(&self) -> usize {
        self.text.len()
    }

    /// The notification that tells a peer this generation can be fetched.
    #[must_use]
    pub fn announce(&self) -> proto::ClipboardAnnounce {
        proto::ClipboardAnnounce {
            sequence: self.sequence,
            formats: vec![proto::ClipboardFormat {
                mime: TEXT_MIME.to_owned(),
                size_bytes: wire_size(self.text.len()),
                sha256: Some(self.digest.0.to_vec()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }
}

impl fmt::Debug for TextOffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TextOffer")
            .field("sequence", &self.sequence)
            .field("size_bytes", &self.text.len())
            .finish_non_exhaustive()
    }
}

/// What a peer's `ClipboardAnnounce` offers this implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Announced {
    /// A text representation within [`MAX_TEXT_BYTES`].
    Text(AnnouncedText),
    /// A text representation larger than [`MAX_TEXT_BYTES`]; it is not
    /// fetched.
    TooLarge,
    /// No usable text representation: other formats only, empty text, or a
    /// malformed digest.
    NoText,
}

impl Announced {
    /// Reads the text representation out of `announce`.
    #[must_use]
    pub fn from_announce(announce: &proto::ClipboardAnnounce) -> Self {
        let Some(format) = announce
            .formats
            .iter()
            .find(|format| is_text_mime(&format.mime))
        else {
            return Self::NoText;
        };
        let digest = match format.sha256.as_deref().map(TextDigest::from_wire) {
            Some(None) => return Self::NoText,
            digest => digest.flatten(),
        };
        match usize::try_from(format.size_bytes) {
            Ok(0) => Self::NoText,
            Ok(size) if size <= MAX_TEXT_BYTES => Self::Text(AnnouncedText {
                sequence: announce.sequence,
                size,
                digest,
            }),
            _ => Self::TooLarge,
        }
    }
}

/// An announced text representation, validated for fetching.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnnouncedText {
    sequence: u64,
    size: usize,
    digest: Option<TextDigest>,
}

impl AnnouncedText {
    /// The announced clipboard generation.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// The announced size in UTF-8 bytes.
    #[must_use]
    pub const fn size_bytes(&self) -> usize {
        self.size
    }

    /// The announced digest, when the peer sent one.
    #[must_use]
    pub const fn digest(&self) -> Option<TextDigest> {
        self.digest
    }
}

/// Why fetching announced text failed.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The stream or connection failed, or negotiation gates the fetch.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The peer answered with an `Error` frame carrying this code.
    #[error("peer refused the clipboard fetch: {0:?}")]
    Refused(EnumValue<proto::ErrorCode>),
    /// The peer's response contradicts the announce or the frame contract.
    #[error("peer sent an invalid clipboard transfer: {0}")]
    Invalid(&'static str),
}

/// Fetches an announced text representation and verifies it against the
/// announce: same generation, same size, matching digest, valid UTF-8.
pub async fn fetch_text(
    connection: &FlowConnection,
    announced: &AnnouncedText,
) -> Result<String, FetchError> {
    let request = message_envelope(
        FrameKind::ClipboardFetch,
        &proto::ClipboardFetch {
            sequence: announced.sequence,
            mime: TEXT_MIME.to_owned(),
            offset: 0,
            ..Default::default()
        },
    )?;
    let mut response = match connection.call_bulk(request).await? {
        BulkRpcResponse::Error(error) => {
            let error = decode::<proto::Error>(&error)?;
            return Err(FetchError::Refused(error.code));
        }
        BulkRpcResponse::Data(response) => response,
    };
    let head = decode::<proto::ClipboardData>(response.head())?;
    if head.sequence != announced.sequence || !is_text_mime(&head.mime) {
        return Err(FetchError::Invalid(
            "response names another clipboard generation or representation",
        ));
    }
    if usize::try_from(head.total_size).ok() != Some(announced.size) {
        return Err(FetchError::Invalid(
            "response size differs from the announce",
        ));
    }

    let mut bytes = Vec::with_capacity(announced.size);
    let end = loop {
        let Some(frame) = response.next_frame().await? else {
            return Err(FetchError::Invalid("response ended without ChunkEnd"));
        };
        if frame.kind == FrameKind::ChunkEnd {
            break decode::<proto::ChunkEnd>(&frame)?;
        }
        let chunk = decode::<proto::Chunk>(&frame)?;
        if chunk.data.len() > announced.size - bytes.len() {
            return Err(FetchError::Invalid(
                "response carries more text than announced",
            ));
        }
        bytes.extend_from_slice(&chunk.data);
    };
    if bytes.len() != announced.size {
        return Err(FetchError::Invalid(
            "response carries less text than announced",
        ));
    }
    let digest = TextDigest::of_bytes(&bytes);
    let end_digest = match end.sha256.as_deref().map(TextDigest::from_wire) {
        Some(None) => return Err(FetchError::Invalid("ChunkEnd digest is not SHA-256")),
        end_digest => end_digest.flatten(),
    };
    if [end_digest, announced.digest]
        .into_iter()
        .flatten()
        .any(|expected| expected != digest)
    {
        return Err(FetchError::Invalid("text digest mismatch"));
    }
    String::from_utf8(bytes).map_err(|_| FetchError::Invalid("text is not UTF-8"))
}

/// How [`serve_fetch`] answered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Served {
    /// The whole text of this generation was written to the stream.
    Sent {
        /// The generation that was sent.
        sequence: u64,
    },
    /// The fetch named a generation or representation this machine no longer
    /// offers; it was answered `Error INVALID`.
    Refused,
}

/// Answers one `ClipboardFetch` from `offer`, the generation this machine
/// offers right now.
///
/// A fetch for any other generation, for another representation, or past the
/// end of the text is answered `Error INVALID`: that data is gone.
pub async fn serve_fetch(
    rpc: IncomingRpc,
    offer: Option<&TextOffer>,
) -> Result<Served, TransportError> {
    let kind = rpc.request().kind;
    if kind != FrameKind::ClipboardFetch {
        return Err(TransportError::InvalidRequestKind(kind));
    }
    let fetch = rpc
        .request()
        .decode::<proto::ClipboardFetch>(InboundRole::Request)
        .map_err(|_| TransportError::InvalidPayload(kind))?;
    let offered =
        offer.filter(|offer| offer.sequence == fetch.sequence && is_text_mime(&fetch.mime));
    let Some((offer, start)) = offered
        .zip(usize::try_from(fetch.offset).ok())
        .filter(|(offer, start)| *start <= offer.text.len())
    else {
        rpc.respond(error_envelope(
            proto::ErrorCode::Invalid,
            "clipboard generation or representation is not offered",
        )?)
        .await?;
        return Ok(Served::Refused);
    };

    let bytes = &offer.text.as_bytes()[start..];
    let head = message_envelope(
        FrameKind::ClipboardData,
        &proto::ClipboardData {
            sequence: offer.sequence,
            mime: TEXT_MIME.to_owned(),
            total_size: wire_size(offer.text.len()),
            ..Default::default()
        },
    )?;
    let chunks = bytes
        .chunks(CHUNK_TEXT_LEN)
        .map(|data| {
            message_envelope(
                FrameKind::Chunk,
                &proto::Chunk {
                    data: data.to_vec(),
                    ..Default::default()
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let end = message_envelope(
        FrameKind::ChunkEnd,
        &proto::ChunkEnd {
            sha256: Some(TextDigest::of_bytes(bytes).0.to_vec()),
            ..Default::default()
        },
    )?;
    rpc.respond_bulk(head, chunks, end).await?;
    Ok(Served::Sent {
        sequence: offer.sequence,
    })
}

/// Media-type match for [`TEXT_MIME`]: case-insensitive, ignoring the
/// optional whitespace around parameters.
fn is_text_mime(mime: &str) -> bool {
    let mut normalized = mime
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .map(|character| character.to_ascii_lowercase());
    TEXT_MIME
        .chars()
        .all(|expected| normalized.next() == Some(expected))
        && normalized.next().is_none()
}

fn wire_size(len: usize) -> u64 {
    u64::try_from(len).unwrap_or_else(|_| unreachable!("text is bounded by MAX_TEXT_BYTES"))
}

fn decode<M: buffa::Message>(envelope: &Envelope) -> Result<M, FetchError> {
    envelope
        .decode(InboundRole::Request)
        .map_err(|_| FetchError::Invalid("frame payload does not decode"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announce_of(formats: Vec<proto::ClipboardFormat>) -> proto::ClipboardAnnounce {
        proto::ClipboardAnnounce {
            sequence: 9,
            formats,
            ..Default::default()
        }
    }

    fn format(mime: &str, size_bytes: u64, sha256: Option<Vec<u8>>) -> proto::ClipboardFormat {
        proto::ClipboardFormat {
            mime: mime.to_owned(),
            size_bytes,
            sha256,
            ..Default::default()
        }
    }

    #[test]
    fn an_offer_announces_its_text_by_size_and_digest() {
        let offer = TextOffer::new(3, "héllo".to_owned()).unwrap();
        let announce = offer.announce();

        assert_eq!(announce.sequence, 3);
        assert_eq!(announce.formats.len(), 1);
        assert_eq!(announce.formats[0].mime, TEXT_MIME);
        assert_eq!(announce.formats[0].size_bytes, 6);
        assert_eq!(
            Announced::from_announce(&announce),
            Announced::Text(AnnouncedText {
                sequence: 3,
                size: 6,
                digest: Some(TextDigest::of("héllo")),
            })
        );
    }

    #[test]
    fn empty_or_oversized_text_is_never_offered() {
        assert_eq!(
            TextOffer::new(1, String::new()).unwrap_err(),
            OfferError::Empty
        );
        assert_eq!(
            TextOffer::new(1, "a".repeat(MAX_TEXT_BYTES + 1)).unwrap_err(),
            OfferError::TooLarge
        );
        assert_eq!(
            TextOffer::new(1, "a".repeat(MAX_TEXT_BYTES))
                .unwrap()
                .size_bytes(),
            MAX_TEXT_BYTES
        );
    }

    #[test]
    fn announces_without_fetchable_text_are_classified() {
        let over = wire_size(MAX_TEXT_BYTES) + 1;
        assert_eq!(
            Announced::from_announce(&announce_of(vec![format(TEXT_MIME, over, None)])),
            Announced::TooLarge
        );
        assert_eq!(
            Announced::from_announce(&announce_of(vec![format("image/png", 10, None)])),
            Announced::NoText
        );
        assert_eq!(
            Announced::from_announce(&announce_of(Vec::new())),
            Announced::NoText
        );
        assert_eq!(
            Announced::from_announce(&announce_of(vec![format(TEXT_MIME, 0, None)])),
            Announced::NoText
        );
        assert_eq!(
            Announced::from_announce(&announce_of(vec![format(TEXT_MIME, 4, Some(vec![0; 31]))])),
            Announced::NoText
        );
    }

    #[test]
    fn the_text_representation_matches_media_type_spelling_variants() {
        for mime in [
            TEXT_MIME,
            "text/plain; charset=utf-8",
            "Text/Plain;Charset=UTF-8",
        ] {
            assert!(is_text_mime(mime), "{mime} names UTF-8 text");
        }
        for mime in ["text/plain", "text/plain;charset=utf-16", "text/html"] {
            assert!(!is_text_mime(mime), "{mime} is not the UTF-8 text mime");
        }
        assert_eq!(
            Announced::from_announce(&announce_of(vec![
                format("image/png", 10, None),
                format("text/plain; charset=UTF-8", 4, None),
            ])),
            Announced::Text(AnnouncedText {
                sequence: 9,
                size: 4,
                digest: None,
            })
        );
    }

    #[test]
    fn a_full_chunk_fits_the_chunk_frame_cap() {
        let chunk = message_envelope(
            FrameKind::Chunk,
            &proto::Chunk {
                data: vec![0xa5; CHUNK_TEXT_LEN],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(chunk.payload.len(), MAX_CHUNK_LEN);
        assert!(
            message_envelope(
                FrameKind::Chunk,
                &proto::Chunk {
                    data: vec![0xa5; CHUNK_TEXT_LEN + 1],
                    ..Default::default()
                },
            )
            .is_err(),
            "one more byte must overflow the chunk cap"
        );
    }

    #[test]
    fn debug_output_never_carries_the_text() {
        let offer = TextOffer::new(5, "hunter2".to_owned()).unwrap();
        let printed = format!("{offer:?} {:?}", offer.digest());
        assert!(!printed.contains("hunter2"));
        assert!(printed.contains("size_bytes: 7"));
        assert!(printed.contains("TextDigest(..)"));
    }
}
