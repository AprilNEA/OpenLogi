//! Clipboard transfer over the capability-gated clipboard family: text, and
//! files.
//!
//! The holder of a clipboard generation announces it
//! ([`ClipboardOffer::announce`]); a peer that wants it fetches it
//! ([`fetch_text`], or [`fetch_file_list`] and then [`fetch_file`] for each
//! file), and the holder answers from whatever generation it offers at that
//! moment ([`serve_fetch`]). The bytes ride the fetch's own stream as
//! `ClipboardData`, `Chunk`*, `ChunkEnd` (FLOW-PROTOCOL.md §Clipboard & files).
//!
//! Text is UTF-8 under [`TEXT_MIME`], bounded by [`MAX_TEXT_BYTES`] on both
//! ends. Files are announced as a `FileList` under [`FILES_MIME`], bounded by
//! [`MAX_FILES`] and [`MAX_FILES_BYTES`], and both ends check every path with
//! [`is_safe_relative_path`]. None of these types print clipboard contents or
//! file names: their `Debug` output names sizes and sequences only.

use std::collections::HashSet;
use std::fmt;
use std::io::{self, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use buffa::{EnumValue, Message as _};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::{
    frame::{Envelope, FrameKind, InboundRole, MAX_CHUNK_LEN},
    generated as proto,
    negotiation::Capability,
    transport::{
        BulkResponse, BulkRpcResponse, FlowConnection, IncomingRpc, TransportError, error_envelope,
        message_envelope,
    },
};

/// Media type of the UTF-8 text representation.
pub const TEXT_MIME: &str = "text/plain;charset=utf-8";

/// Media type of the file-list representation; its payload is a `FileList`.
pub const FILES_MIME: &str = "application/x-openlogi-files";

/// Media type a `FileFetch` response names for one file's bytes.
pub const FILE_DATA_MIME: &str = "application/octet-stream";

/// The most UTF-8 bytes of text offered or accepted. A larger clipboard is
/// skipped whole, never truncated.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;

/// The most files one clipboard generation may carry.
pub const MAX_FILES: usize = 1000;

/// The most bytes one clipboard generation's files may hold in all. Files are
/// fetched as soon as they are announced, so this bounds what a crossing can
/// cost.
pub const MAX_FILES_BYTES: u64 = 1 << 30;

/// The longest relative path a file list may name, in UTF-8 bytes.
pub const MAX_RELATIVE_PATH_BYTES: usize = 1024;

/// The longest encoded file list accepted: [`MAX_FILES`] paths of a typical
/// length, with room to spare.
const MAX_FILE_LIST_BYTES: usize = 2 * 1024 * 1024;

/// Payload bytes per `Chunk`: the frame cap minus the protobuf key (1 byte)
/// and length prefix (3 bytes below 2^21) that frame `Chunk.data`.
const CHUNK_DATA_LEN: usize = MAX_CHUNK_LEN - 4;

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

/// Whether `connection` negotiated file clipboard sync: both peers listed
/// `CAPABILITY_CLIPBOARD_FILES` as well as `CAPABILITY_CLIPBOARD_TEXT`, whose
/// frames carry the announce and the file list.
#[must_use]
pub fn files_sync_enabled(connection: &FlowConnection) -> bool {
    let capabilities = &connection.negotiated().capabilities;
    capabilities.contains(&Capability::ClipboardText)
        && capabilities.contains(&Capability::ClipboardFiles)
}

/// SHA-256 of offered content: a text's UTF-8 bytes, or a file list's
/// encoding.
///
/// Announces and transfers carry it so a receiver can verify what it
/// assembled and skip content it already holds. `Debug` redacts the bytes: the
/// digest of a short secret is as good as the secret.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
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

impl fmt::Debug for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ContentDigest(..)")
    }
}

/// Why clipboard content cannot be offered.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OfferError {
    /// There is no text to place on a peer's clipboard.
    #[error("clipboard text is empty")]
    Empty,
    /// The text exceeds [`MAX_TEXT_BYTES`].
    #[error("clipboard text exceeds the {MAX_TEXT_BYTES}-byte sync limit")]
    TooLarge,
    /// There are no files to place on a peer's clipboard.
    #[error("no clipboard files")]
    NoFiles,
    /// More than [`MAX_FILES`] files and empty folders, or more than
    /// [`MAX_FILES_BYTES`] bytes.
    #[error(
        "clipboard files exceed the sync limit of {MAX_FILES} files and {MAX_FILES_BYTES} bytes"
    )]
    FilesTooLarge,
    /// A file's path fails [`is_safe_relative_path`], or two paths collide.
    #[error("a clipboard file's name cannot be sent")]
    UnsafePath,
}

/// One clipboard generation's text, as this machine offers it to peers.
pub struct TextOffer {
    sequence: u64,
    text: String,
    digest: ContentDigest,
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
        let digest = ContentDigest::of(&text);
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
    pub const fn digest(&self) -> ContentDigest {
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
        announce_one(self.sequence, TEXT_MIME, self.text.len(), self.digest)
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

/// One file this machine offers, and where its bytes are read from.
#[derive(Clone, Eq, PartialEq)]
pub struct OfferedFile {
    /// Where the file sits among the copied items, `/`-separated; see
    /// [`is_safe_relative_path`].
    pub relative_path: String,
    /// Its size when it was offered. A fetch of a file whose size changed
    /// since is refused.
    pub size_bytes: u64,
    /// When it was last modified, in Unix milliseconds; 0 when unknown.
    pub modified_at_ms: u64,
    /// The local file a fetch reads.
    pub source: PathBuf,
}

impl fmt::Debug for OfferedFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OfferedFile")
            .field("size_bytes", &self.size_bytes)
            .finish_non_exhaustive()
    }
}

/// One clipboard generation's files, as this machine offers them to peers.
pub struct FilesOffer {
    sequence: u64,
    files: Vec<OfferedFile>,
    /// The encoded `FileList` a peer fetches first.
    list: Vec<u8>,
    digest: ContentDigest,
    total_bytes: u64,
}

impl FilesOffer {
    /// Wraps the files of clipboard generation `sequence`, in the order a
    /// peer fetches them by index.
    pub fn new(sequence: u64, files: Vec<OfferedFile>) -> Result<Self, OfferError> {
        Self::with_folders(sequence, files, Vec::new())
    }

    /// Wraps the files of clipboard generation `sequence`, in the order a
    /// peer fetches them by index, and the copied `folders` that hold no
    /// file, `/`-separated like the files' paths.
    pub fn with_folders(
        sequence: u64,
        files: Vec<OfferedFile>,
        folders: Vec<String>,
    ) -> Result<Self, OfferError> {
        if files.is_empty() && folders.is_empty() {
            return Err(OfferError::NoFiles);
        }
        let total_bytes = total_size(files.iter().map(|file| file.size_bytes))
            .filter(|_| files.len() + folders.len() <= MAX_FILES)
            .ok_or(OfferError::FilesTooLarge)?;
        if !paths_can_coexist(
            files
                .iter()
                .map(|file| file.relative_path.as_str())
                .chain(folders.iter().map(String::as_str)),
        ) {
            return Err(OfferError::UnsafePath);
        }
        let list = proto::FileList {
            files: files
                .iter()
                .map(|file| proto::FileEntry {
                    relative_path: file.relative_path.clone(),
                    size_bytes: file.size_bytes,
                    modified_at_ms: file.modified_at_ms,
                    ..Default::default()
                })
                .collect(),
            folders,
            ..Default::default()
        }
        .try_encode_to_vec()
        .ok()
        .filter(|list| list.len() <= MAX_FILE_LIST_BYTES)
        .ok_or(OfferError::FilesTooLarge)?;
        let digest = ContentDigest::of_bytes(&list);
        Ok(Self {
            sequence,
            files,
            list,
            digest,
            total_bytes,
        })
    }

    /// The clipboard generation these files belong to.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// The digest of the file list: paths, sizes and modification times. Two
    /// machines holding the same files compute the same one.
    #[must_use]
    pub const fn digest(&self) -> ContentDigest {
        self.digest
    }

    /// How many files are offered.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// The offered files' combined size.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// The notification that tells a peer this generation can be fetched.
    #[must_use]
    pub fn announce(&self) -> proto::ClipboardAnnounce {
        announce_one(self.sequence, FILES_MIME, self.list.len(), self.digest)
    }
}

impl fmt::Debug for FilesOffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FilesOffer")
            .field("sequence", &self.sequence)
            .field("files", &self.files.len())
            .field("total_bytes", &self.total_bytes)
            .finish_non_exhaustive()
    }
}

/// One clipboard generation this machine offers: its text, or its files.
#[derive(Debug)]
pub enum ClipboardOffer {
    /// Copied text.
    Text(TextOffer),
    /// Copied files.
    Files(FilesOffer),
}

impl ClipboardOffer {
    /// The clipboard generation offered.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        match self {
            Self::Text(text) => text.sequence,
            Self::Files(files) => files.sequence,
        }
    }

    /// The digest that identifies the offered content.
    #[must_use]
    pub const fn digest(&self) -> ContentDigest {
        match self {
            Self::Text(text) => text.digest,
            Self::Files(files) => files.digest,
        }
    }

    /// The notification that tells a peer this generation can be fetched.
    #[must_use]
    pub fn announce(&self) -> proto::ClipboardAnnounce {
        match self {
            Self::Text(text) => text.announce(),
            Self::Files(files) => files.announce(),
        }
    }
}

/// What a peer's `ClipboardAnnounce` offers this implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Announced {
    /// A text representation within [`MAX_TEXT_BYTES`].
    Text(AnnouncedText),
    /// A file list, fetched with [`fetch_file_list`].
    Files(AnnouncedFiles),
    /// Text over [`MAX_TEXT_BYTES`], or a file list too long to be within
    /// [`MAX_FILES`]; it is not fetched.
    TooLarge,
    /// Nothing usable: other formats only, an empty representation, or a
    /// malformed digest.
    Nothing,
}

impl Announced {
    /// Reads the representation this implementation fetches out of
    /// `announce`: files when listed, text otherwise.
    #[must_use]
    pub fn from_announce(announce: &proto::ClipboardAnnounce) -> Self {
        let files = announce
            .formats
            .iter()
            .find(|format| format.mime.eq_ignore_ascii_case(FILES_MIME));
        if let Some(format) = files {
            return match announced_size(format, MAX_FILE_LIST_BYTES) {
                Ok((size, digest)) => Self::Files(AnnouncedFiles {
                    sequence: announce.sequence,
                    size,
                    digest,
                }),
                Err(other) => other,
            };
        }
        let Some(format) = announce
            .formats
            .iter()
            .find(|format| is_text_mime(&format.mime))
        else {
            return Self::Nothing;
        };
        match announced_size(format, MAX_TEXT_BYTES) {
            Ok((size, digest)) => Self::Text(AnnouncedText {
                sequence: announce.sequence,
                size,
                digest,
            }),
            Err(other) => other,
        }
    }
}

/// A representation's size within `limit`, and its digest when it sent one.
fn announced_size(
    format: &proto::ClipboardFormat,
    limit: usize,
) -> Result<(usize, Option<ContentDigest>), Announced> {
    let digest = match format.sha256.as_deref().map(ContentDigest::from_wire) {
        Some(None) => return Err(Announced::Nothing),
        digest => digest.flatten(),
    };
    match usize::try_from(format.size_bytes) {
        Ok(0) => Err(Announced::Nothing),
        Ok(size) if size <= limit => Ok((size, digest)),
        _ => Err(Announced::TooLarge),
    }
}

/// An announced text representation, validated for fetching.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnnouncedText {
    sequence: u64,
    size: usize,
    digest: Option<ContentDigest>,
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
    pub const fn digest(&self) -> Option<ContentDigest> {
        self.digest
    }
}

/// An announced file list, validated for fetching.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnnouncedFiles {
    sequence: u64,
    size: usize,
    digest: Option<ContentDigest>,
}

impl AnnouncedFiles {
    /// The announced clipboard generation.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// The digest of the file list, when the peer sent one: equal to the
    /// [`FilesOffer::digest`] of the same files offered here.
    #[must_use]
    pub const fn digest(&self) -> Option<ContentDigest> {
        self.digest
    }
}

/// A peer's file list, checked: its files and the folders that hold none,
/// all safe to create side by side under a folder of this machine's choosing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileListing {
    files: Vec<ListedFile>,
    folders: Vec<ListedFolder>,
}

impl FileListing {
    /// The files, in the order a fetch names them by index.
    #[must_use]
    pub fn files(&self) -> &[ListedFile] {
        &self.files
    }

    /// The folders that hold no file.
    #[must_use]
    pub fn folders(&self) -> &[ListedFolder] {
        &self.folders
    }
}

/// A folder of a peer's file list that holds no file.
#[derive(Clone, Eq, PartialEq)]
pub struct ListedFolder(String);

impl ListedFolder {
    /// Where the folder goes, `/`-separated; it passed
    /// [`is_safe_relative_path`].
    #[must_use]
    pub fn relative_path(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ListedFolder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ListedFolder")
            .finish_non_exhaustive()
    }
}

/// One file of a peer's file list, checked to be safe to create under a
/// folder of this machine's choosing.
#[derive(Clone, Eq, PartialEq)]
pub struct ListedFile {
    relative_path: String,
    size_bytes: u64,
    modified_at_ms: u64,
}

impl ListedFile {
    /// Where the file goes, `/`-separated; it passed
    /// [`is_safe_relative_path`].
    #[must_use]
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// The file's size.
    #[must_use]
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// When the file was last modified, in Unix milliseconds; 0 when
    /// unknown.
    #[must_use]
    pub const fn modified_at_ms(&self) -> u64 {
        self.modified_at_ms
    }
}

impl fmt::Debug for ListedFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ListedFile")
            .field("size_bytes", &self.size_bytes)
            .finish_non_exhaustive()
    }
}

/// Why fetching announced content failed.
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
    /// The peer sent nothing for the whole stall timeout.
    #[error("the clipboard transfer stalled")]
    Stalled,
    /// The caller stopped wanting the transfer.
    #[error("the clipboard transfer was superseded")]
    Superseded,
    /// Writing the fetched bytes failed.
    #[error("writing the clipboard transfer failed: {0}")]
    Write(#[source] io::Error),
}

/// Fetches an announced text representation and verifies it against the
/// announce: same generation, same size, matching digest, valid UTF-8.
pub async fn fetch_text(
    connection: &FlowConnection,
    announced: &AnnouncedText,
) -> Result<String, FetchError> {
    let bytes = fetch_payload(
        connection,
        announced.sequence,
        TEXT_MIME,
        announced.size,
        announced.digest,
    )
    .await?;
    String::from_utf8(bytes).map_err(|_| FetchError::Invalid("text is not UTF-8"))
}

/// Fetches an announced file list and checks it: within [`MAX_FILES`] and
/// [`MAX_FILES_BYTES`], and every path safe and distinct from the others.
pub async fn fetch_file_list(
    connection: &FlowConnection,
    announced: &AnnouncedFiles,
) -> Result<FileListing, FetchError> {
    let bytes = fetch_payload(
        connection,
        announced.sequence,
        FILES_MIME,
        announced.size,
        announced.digest,
    )
    .await?;
    let list = proto::FileList::decode_from_slice(&bytes)
        .map_err(|_| FetchError::Invalid("file list does not decode"))?;
    validate_file_list(list).map_err(FetchError::Invalid)
}

/// Streams file `index` of clipboard generation `sequence` into `sink`, and
/// checks it against `file`: same size, and the digest the peer sends at the
/// end. `still_wanted` is asked before every chunk, and a no stops the fetch
/// with [`FetchError::Superseded`]; a peer silent for `stall`, before its
/// response opens or between frames, fails it with [`FetchError::Stalled`]. What was written to `sink` before a failure is
/// the caller's to discard.
pub async fn fetch_file(
    connection: &FlowConnection,
    sequence: u64,
    index: u32,
    file: &ListedFile,
    sink: &mut (impl AsyncWrite + Unpin),
    stall: Duration,
    still_wanted: impl Fn() -> bool,
) -> Result<(), FetchError> {
    let request = message_envelope(
        FrameKind::FileFetch,
        &proto::FileFetch {
            sequence,
            file_index: index,
            offset: 0,
            ..Default::default()
        },
    )?;
    let mut response = tokio::time::timeout(stall, call_bulk(connection, request))
        .await
        .map_err(|_| FetchError::Stalled)??;
    let head = decode::<proto::ClipboardData>(response.head())?;
    if head.sequence != sequence || !head.mime.eq_ignore_ascii_case(FILE_DATA_MIME) {
        return Err(FetchError::Invalid(
            "response names another clipboard generation or representation",
        ));
    }
    if head.total_size != file.size_bytes {
        return Err(FetchError::Invalid(
            "response size differs from the file list",
        ));
    }

    let mut hasher = Sha256::new();
    let mut received = 0_u64;
    let end = loop {
        if !still_wanted() {
            return Err(FetchError::Superseded);
        }
        let frame = tokio::time::timeout(stall, response.next_frame())
            .await
            .map_err(|_| FetchError::Stalled)??
            .ok_or(FetchError::Invalid("response ended without ChunkEnd"))?;
        if frame.kind == FrameKind::ChunkEnd {
            break decode::<proto::ChunkEnd>(&frame)?;
        }
        let chunk = decode::<proto::Chunk>(&frame)?;
        let len = wire_size(chunk.data.len());
        if len > file.size_bytes - received {
            return Err(FetchError::Invalid(
                "response carries more bytes than the file has",
            ));
        }
        hasher.update(&chunk.data);
        sink.write_all(&chunk.data)
            .await
            .map_err(FetchError::Write)?;
        received += len;
    };
    if received != file.size_bytes {
        return Err(FetchError::Invalid(
            "response carries fewer bytes than the file has",
        ));
    }
    check_end_digest(&end, ContentDigest(hasher.finalize().into()), None)?;
    sink.flush().await.map_err(FetchError::Write)
}

/// Fetches one whole representation of generation `sequence` into memory and
/// checks it against its announce.
async fn fetch_payload(
    connection: &FlowConnection,
    sequence: u64,
    mime: &str,
    size: usize,
    digest: Option<ContentDigest>,
) -> Result<Vec<u8>, FetchError> {
    let request = message_envelope(
        FrameKind::ClipboardFetch,
        &proto::ClipboardFetch {
            sequence,
            mime: mime.to_owned(),
            offset: 0,
            ..Default::default()
        },
    )?;
    let mut response = call_bulk(connection, request).await?;
    let head = decode::<proto::ClipboardData>(response.head())?;
    let same_mime = if mime == TEXT_MIME {
        is_text_mime(&head.mime)
    } else {
        head.mime.eq_ignore_ascii_case(mime)
    };
    if head.sequence != sequence || !same_mime {
        return Err(FetchError::Invalid(
            "response names another clipboard generation or representation",
        ));
    }
    if usize::try_from(head.total_size).ok() != Some(size) {
        return Err(FetchError::Invalid(
            "response size differs from the announce",
        ));
    }

    let mut bytes = Vec::with_capacity(size);
    let end = loop {
        let Some(frame) = response.next_frame().await? else {
            return Err(FetchError::Invalid("response ended without ChunkEnd"));
        };
        if frame.kind == FrameKind::ChunkEnd {
            break decode::<proto::ChunkEnd>(&frame)?;
        }
        let chunk = decode::<proto::Chunk>(&frame)?;
        if chunk.data.len() > size - bytes.len() {
            return Err(FetchError::Invalid(
                "response carries more bytes than announced",
            ));
        }
        bytes.extend_from_slice(&chunk.data);
    };
    if bytes.len() != size {
        return Err(FetchError::Invalid(
            "response carries fewer bytes than announced",
        ));
    }
    check_end_digest(&end, ContentDigest::of_bytes(&bytes), digest)?;
    Ok(bytes)
}

async fn call_bulk(
    connection: &FlowConnection,
    request: Envelope,
) -> Result<BulkResponse, FetchError> {
    match connection.call_bulk(request).await? {
        BulkRpcResponse::Error(error) => {
            let error = decode::<proto::Error>(&error)?;
            Err(FetchError::Refused(error.code))
        }
        BulkRpcResponse::Data(response) => Ok(response),
    }
}

/// Checks `received` against the digest `ChunkEnd` carries and the one the
/// announce carried, each when present.
fn check_end_digest(
    end: &proto::ChunkEnd,
    received: ContentDigest,
    announced: Option<ContentDigest>,
) -> Result<(), FetchError> {
    let end_digest = match end.sha256.as_deref().map(ContentDigest::from_wire) {
        Some(None) => return Err(FetchError::Invalid("ChunkEnd digest is not SHA-256")),
        end_digest => end_digest.flatten(),
    };
    if [end_digest, announced]
        .into_iter()
        .flatten()
        .any(|expected| expected != received)
    {
        return Err(FetchError::Invalid("content digest mismatch"));
    }
    Ok(())
}

/// A peer's file list as this machine accepts it, or why not.
fn validate_file_list(list: proto::FileList) -> Result<FileListing, &'static str> {
    if list.files.is_empty() && list.folders.is_empty() {
        return Err("the file list is empty");
    }
    if list.files.len() + list.folders.len() > MAX_FILES
        || total_size(list.files.iter().map(|file| file.size_bytes)).is_none()
    {
        return Err("the files exceed the sync limit");
    }
    if !paths_can_coexist(
        list.files
            .iter()
            .map(|file| file.relative_path.as_str())
            .chain(list.folders.iter().map(String::as_str)),
    ) {
        return Err("the file list names an unsafe or colliding path");
    }
    Ok(FileListing {
        files: list
            .files
            .into_iter()
            .map(|file| ListedFile {
                relative_path: file.relative_path,
                size_bytes: file.size_bytes,
                modified_at_ms: file.modified_at_ms,
            })
            .collect(),
        folders: list.folders.into_iter().map(ListedFolder).collect(),
    })
}

/// The sum of `sizes`, when it is within [`MAX_FILES_BYTES`].
fn total_size(sizes: impl IntoIterator<Item = u64>) -> Option<u64> {
    sizes
        .into_iter()
        .try_fold(0_u64, u64::checked_add)
        .filter(|total| *total <= MAX_FILES_BYTES)
}

/// Whether these paths can all be created side by side: each one safe, none
/// repeated (ignoring case, as Windows and macOS do), and none the folder of
/// another.
fn paths_can_coexist<'a>(paths: impl IntoIterator<Item = &'a str>) -> bool {
    let mut files = HashSet::new();
    let mut folders = HashSet::new();
    for path in paths {
        if !is_safe_relative_path(path) {
            return false;
        }
        let path = path.to_lowercase();
        let mut rest = path.as_str();
        while let Some((folder, _)) = rest.rsplit_once('/') {
            folders.insert(folder.to_owned());
            rest = folder;
        }
        if !files.insert(path) {
            return false;
        }
    }
    files.is_disjoint(&folders)
}

/// Whether `path` can name a file under a folder of the receiver's choosing,
/// on any platform: relative, `/`-separated, within
/// [`MAX_RELATIVE_PATH_BYTES`], and every segment a name Windows accepts — not
/// `.` or `..`, not a reserved device name such as `CON` or `LPT1`, none of
/// `\ : * ? " < > |` or control characters, and no trailing dot or space.
#[must_use]
pub fn is_safe_relative_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= MAX_RELATIVE_PATH_BYTES && path.split('/').all(is_safe_name)
}

fn is_safe_name(name: &str) -> bool {
    const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    const NUMBERED: [&str; 2] = ["COM", "LPT"];
    if name.is_empty()
        || name.len() > 255
        || name.ends_with(['.', ' '])
        || name.chars().any(|character| {
            character.is_control()
                || matches!(character, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
    {
        return false;
    }
    // A device name stays reserved with any extension: `nul.txt` is NUL.
    let stem = name.split('.').next().unwrap_or(name);
    let reserved = RESERVED
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
        || NUMBERED.iter().any(|device| {
            // `get`, not indexing: a 4-byte stem such as `a€` has no char
            // boundary at 3.
            stem.len() == 4
                && stem
                    .get(..3)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(device))
                && matches!(stem.as_bytes()[3], b'1'..=b'9')
        });
    !reserved
}

/// How [`serve_fetch`] answered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Served {
    /// The whole text of this generation was written to the stream.
    Text {
        /// The generation that was sent.
        sequence: u64,
    },
    /// This generation's file list, or one of its files, was written.
    Files {
        /// The generation that was sent.
        sequence: u64,
    },
    /// The fetch named a generation, representation or file this machine no
    /// longer offers, or a file that changed since; it was answered
    /// `Error INVALID`.
    Refused,
}

/// Answers one `ClipboardFetch` or `FileFetch` from `offer`, the generation
/// this machine offers right now.
///
/// A fetch for any other generation, for a representation or file it does
/// not offer, past the end of the data, or for a file whose size changed
/// since it was offered is answered `Error INVALID`: that data is gone.
pub async fn serve_fetch(
    rpc: IncomingRpc,
    offer: Option<&ClipboardOffer>,
) -> Result<Served, TransportError> {
    let kind = rpc.request().kind;
    match kind {
        FrameKind::ClipboardFetch => {
            let fetch = rpc
                .request()
                .decode::<proto::ClipboardFetch>(InboundRole::Request)
                .map_err(|_| TransportError::InvalidPayload(kind))?;
            match offer.filter(|offer| offer.sequence() == fetch.sequence) {
                Some(ClipboardOffer::Text(text)) if is_text_mime(&fetch.mime) => Ok(
                    if serve_bytes(
                        rpc,
                        text.sequence,
                        TEXT_MIME,
                        text.text.as_bytes(),
                        fetch.offset,
                    )
                    .await?
                    {
                        Served::Text {
                            sequence: text.sequence,
                        }
                    } else {
                        Served::Refused
                    },
                ),
                Some(ClipboardOffer::Files(files))
                    if fetch.mime.eq_ignore_ascii_case(FILES_MIME) =>
                {
                    Ok(
                        if serve_bytes(rpc, files.sequence, FILES_MIME, &files.list, fetch.offset)
                            .await?
                        {
                            Served::Files {
                                sequence: files.sequence,
                            }
                        } else {
                            Served::Refused
                        },
                    )
                }
                _ => refuse(rpc).await,
            }
        }
        FrameKind::FileFetch => {
            let fetch = rpc
                .request()
                .decode::<proto::FileFetch>(InboundRole::Request)
                .map_err(|_| TransportError::InvalidPayload(kind))?;
            let wanted = match offer {
                Some(ClipboardOffer::Files(files)) if files.sequence == fetch.sequence => {
                    usize::try_from(fetch.file_index)
                        .ok()
                        .and_then(|index| files.files.get(index))
                        .map(|file| (files.sequence, file))
                }
                _ => None,
            };
            match wanted {
                Some((sequence, file)) => serve_file(rpc, sequence, file, fetch.offset).await,
                None => refuse(rpc).await,
            }
        }
        kind => Err(TransportError::InvalidRequestKind(kind)),
    }
}

/// Writes `bytes` from `offset` as a bulk response; `false` when `offset` is
/// past the end and the fetch was refused instead.
async fn serve_bytes(
    rpc: IncomingRpc,
    sequence: u64,
    mime: &str,
    bytes: &[u8],
    offset: u64,
) -> Result<bool, TransportError> {
    let Some(rest) = usize::try_from(offset)
        .ok()
        .and_then(|start| bytes.get(start..))
    else {
        refuse(rpc).await?;
        return Ok(false);
    };
    let head = data_head(sequence, mime, wire_size(bytes.len()))?;
    let chunks = rest
        .chunks(CHUNK_DATA_LEN)
        .map(chunk_envelope)
        .collect::<Result<Vec<_>, _>>()?;
    let end = end_envelope(ContentDigest::of_bytes(rest))?;
    rpc.respond_bulk(head, chunks, end).await?;
    Ok(true)
}

/// Streams `file` from `offset`, read off disk as it is sent.
async fn serve_file(
    rpc: IncomingRpc,
    sequence: u64,
    file: &OfferedFile,
    offset: u64,
) -> Result<Served, TransportError> {
    let Ok(mut source) = open_unchanged(&file.source, file.size_bytes, offset).await else {
        return refuse(rpc).await;
    };
    let mut writer = rpc
        .start_bulk(data_head(sequence, FILE_DATA_MIME, file.size_bytes)?)
        .await?;
    let mut hasher = Sha256::new();
    let mut remaining = file.size_bytes - offset;
    let mut buffer = vec![0; CHUNK_DATA_LEN];
    while remaining > 0 {
        let want =
            usize::try_from(remaining).map_or(CHUNK_DATA_LEN, |left| left.min(CHUNK_DATA_LEN));
        let read = source.read(&mut buffer[..want]).await?;
        if read == 0 {
            // Shrunk while it was sent: dropping the writer resets the stream.
            return Err(TransportError::from(io::Error::from(
                io::ErrorKind::UnexpectedEof,
            )));
        }
        hasher.update(&buffer[..read]);
        writer.chunk(&chunk_envelope(&buffer[..read])?).await?;
        remaining -= wire_size(read);
    }
    writer
        .finish(&end_envelope(ContentDigest(hasher.finalize().into()))?)
        .await?;
    Ok(Served::Files { sequence })
}

/// Opens `path` at `offset`, failing unless it still has the offered `size`.
async fn open_unchanged(path: &Path, size: u64, offset: u64) -> io::Result<tokio::fs::File> {
    let mut file = tokio::fs::File::open(path).await?;
    if file.metadata().await?.len() != size || offset > size {
        return Err(io::Error::other("the file changed since it was offered"));
    }
    file.seek(SeekFrom::Start(offset)).await?;
    Ok(file)
}

async fn refuse(rpc: IncomingRpc) -> Result<Served, TransportError> {
    rpc.respond(error_envelope(
        proto::ErrorCode::Invalid,
        "clipboard generation or representation is not offered",
    )?)
    .await?;
    Ok(Served::Refused)
}

fn announce_one(
    sequence: u64,
    mime: &str,
    size: usize,
    digest: ContentDigest,
) -> proto::ClipboardAnnounce {
    proto::ClipboardAnnounce {
        sequence,
        formats: vec![proto::ClipboardFormat {
            mime: mime.to_owned(),
            size_bytes: wire_size(size),
            sha256: Some(digest.0.to_vec()),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn data_head(sequence: u64, mime: &str, total_size: u64) -> Result<Envelope, TransportError> {
    message_envelope(
        FrameKind::ClipboardData,
        &proto::ClipboardData {
            sequence,
            mime: mime.to_owned(),
            total_size,
            ..Default::default()
        },
    )
}

fn chunk_envelope(data: &[u8]) -> Result<Envelope, TransportError> {
    message_envelope(
        FrameKind::Chunk,
        &proto::Chunk {
            data: data.to_vec(),
            ..Default::default()
        },
    )
}

fn end_envelope(digest: ContentDigest) -> Result<Envelope, TransportError> {
    message_envelope(
        FrameKind::ChunkEnd,
        &proto::ChunkEnd {
            sha256: Some(digest.0.to_vec()),
            ..Default::default()
        },
    )
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
    u64::try_from(len).unwrap_or_else(|_| unreachable!("a usize fits u64 on every target"))
}

fn decode<M: buffa::Message>(envelope: &Envelope) -> Result<M, FetchError> {
    envelope
        .decode(InboundRole::Request)
        .map_err(|_| FetchError::Invalid("frame payload does not decode"))
}

#[cfg(test)]
mod tests;
