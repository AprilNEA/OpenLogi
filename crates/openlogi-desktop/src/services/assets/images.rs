//! Device image and depot-manifest helpers.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use gpui::{
    App, Bounds, Corners, IntoElement, Pixels, RenderImage, RenderOnce, Styled as _, Window,
    canvas, img, point, px, size,
};
use image::imageops::FilterType;
use openlogi_assets::DepotManifest;
use tracing::warn;

/// The leading bytes [`read_image_dimensions`] reads: a WebP `VP8X` or
/// `VP8 ` size ends at byte 30, a PNG one at byte 24.
const DIMENSIONS_HEADER_LEN: usize = 30;

/// Read a render's width and height from its file header.
///
/// The catalog ships renders as PNG and as WebP, many of them under a bare
/// UUID name, so the leading bytes pick the format, not the extension. Only
/// those bytes are read — much cheaper than decoding the whole image.
pub(super) fn read_image_dimensions(path: &Path) -> std::io::Result<(u32, u32)> {
    use std::fs::File;
    use std::io::Read;

    let mut header = Vec::with_capacity(DIMENSIONS_HEADER_LEN);
    File::open(path)?
        .take(DIMENSIONS_HEADER_LEN as u64)
        .read_to_end(&mut header)?;
    png_dimensions(&header)
        .or_else(|| webp_dimensions(&header))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "no PNG or WebP image size in the file header",
            )
        })
}

/// PNG: the 8-byte signature, then the `IHDR` chunk, which is always first.
/// After its 4-byte length and its tag, the data opens with the width and the
/// height as big-endian `u32`s.
fn png_dimensions(header: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

    let ihdr = header
        .strip_prefix(&SIGNATURE)?
        .get(4..)?
        .strip_prefix(b"IHDR")?;
    let (width, rest) = ihdr.split_first_chunk::<4>()?;
    let height = rest.first_chunk::<4>()?;
    Some((u32::from_be_bytes(*width), u32::from_be_bytes(*height)))
}

/// WebP (RFC 9649): a `RIFF` container of form `WEBP`. Its first chunk, after
/// a 4-byte tag and a 4-byte length, carries the size:
/// - `VP8X` (extended format): after 4 bytes of flags, the canvas width and
///   height minus one, each 24-bit little-endian.
/// - `VP8 ` (lossy): after the 3-byte frame tag and the key-frame start code,
///   the width and height as 14-bit little-endian values; the top 2 bits of
///   each `u16` are a scale.
/// - `VP8L` (lossless): after the `0x2f` signature, the width and height minus
///   one, packed as the low two 14-bit fields of a little-endian `u32`.
fn webp_dimensions(header: &[u8]) -> Option<(u32, u32)> {
    const VP8_START_CODE: [u8; 3] = [0x9d, 0x01, 0x2a];
    const VP8L_SIGNATURE: [u8; 1] = [0x2f];

    let form = header
        .strip_prefix(b"RIFF")?
        .get(4..)?
        .strip_prefix(b"WEBP")?;
    let (tag, chunk) = form.split_first_chunk::<4>()?;
    let data = chunk.get(4..)?;
    match tag {
        b"VP8X" => {
            let (width, rest) = data.get(4..)?.split_first_chunk::<3>()?;
            let height = rest.first_chunk::<3>()?;
            Some((u24_le(*width) + 1, u24_le(*height) + 1))
        }
        b"VP8 " => {
            let frame = data.get(3..)?.strip_prefix(&VP8_START_CODE)?;
            let (width, rest) = frame.split_first_chunk::<2>()?;
            let height = rest.first_chunk::<2>()?;
            Some((u14_le(*width), u14_le(*height)))
        }
        b"VP8L" => {
            let bits = data.strip_prefix(&VP8L_SIGNATURE)?.first_chunk::<4>()?;
            let bits = u32::from_le_bytes(*bits);
            Some((
                (bits & FOURTEEN_BITS) + 1,
                ((bits >> 14) & FOURTEEN_BITS) + 1,
            ))
        }
        _ => None,
    }
}

/// The width and height fields of the WebP bitstreams are 14 bits wide.
const FOURTEEN_BITS: u32 = 0x3fff;

fn u24_le([b0, b1, b2]: [u8; 3]) -> u32 {
    u32::from_le_bytes([b0, b1, b2, 0])
}

fn u14_le(bytes: [u8; 2]) -> u32 {
    u32::from(u16::from_le_bytes(bytes)) & FOURTEEN_BITS
}

/// Whether the file at `path` starts like an image format, so the GUI can
/// draw it. The extension says nothing: the catalog names many renders by
/// bare UUID, most of them PNG or WebP. G HUB ships some colour variants
/// encrypted under the same kind of name (the pink and purple G502
/// LIGHTSPEED, #2375), and no decoder reads those.
pub(super) fn is_drawable_image(path: &Path) -> bool {
    use std::fs::File;
    use std::io::Read;

    // `RIFF????WEBP`, the longest signature `guess_format` needs.
    let mut header = Vec::with_capacity(16);
    File::open(path)
        .and_then(|file| file.take(16).read_to_end(&mut header))
        .is_ok_and(|_| image::guess_format(&header).is_ok())
}

/// Look up the colour variant matching `ext` in an already-loaded depot
/// manifest. Returns the `device_image` src filename — falling back to
/// `device_camera_image`, the hero-render key webcam depots use instead — or
/// `None` when the manifest lacks that variant. Pure — the caller loads the
/// manifest once (see [`load_manifest`]) and reuses it across candidate bases.
pub(super) fn variant_image_for(
    manifest: &DepotManifest,
    base_model_id: &str,
    ext: u8,
) -> Option<String> {
    manifest
        .resource_for_variant(base_model_id, ext, "device_image")
        .or_else(|| manifest.resource_for_variant(base_model_id, ext, "device_camera_image"))
        .map(str::to_string)
}

/// Helper that checks variants against candidate model IDs, including the depot name and
/// any stripped `_ext\d+` stem (e.g. for depots like `pro_keyboard_ext1`).
pub(super) fn find_variant_in_manifest<'a, F>(
    manifest: &DepotManifest,
    entry: &'a openlogi_assets::DeviceEntry,
    depot: &'a str,
    ext: u8,
    lookup: F,
) -> Option<String>
where
    F: Fn(&DepotManifest, &str, u8) -> Option<String>,
{
    let candidates = candidate_manifest_bases(entry, depot);
    for base in candidates {
        if let Some(res) = lookup(manifest, &base, ext) {
            return Some(res);
        }
    }
    None
}

/// Enumerate candidate manifest bases: model IDs, the depot name, any stripped `_ext\d+` stem,
/// and the bases of products the catalog files under the same depot (`DEPOT_SIBLINGS`).
pub(super) fn candidate_manifest_bases(
    entry: &openlogi_assets::DeviceEntry,
    depot: &str,
) -> Vec<String> {
    let mut candidates = Vec::new();
    for id in entry.model_id_candidates() {
        if !candidates
            .iter()
            .any(|c: &String| c.eq_ignore_ascii_case(id))
        {
            candidates.push(id.to_string());
        }
    }
    if !candidates
        .iter()
        .any(|c: &String| c.eq_ignore_ascii_case(depot))
    {
        candidates.push(depot.to_string());
    }
    if let Some((stem, suffix)) = depot.rsplit_once("_ext")
        && !suffix.is_empty()
        && suffix.chars().all(|c| c.is_ascii_digit())
        && !candidates
            .iter()
            .any(|c: &String| c.eq_ignore_ascii_case(stem))
    {
        candidates.push(stem.to_string());
    }
    // A sibling's variants keep their own base (`g512_ext6` in the G513
    // depot), tried after the depot's own bases.
    for base in super::depot_sibling_manifest_bases(&entry.display_name) {
        if !candidates
            .iter()
            .any(|c: &String| c.eq_ignore_ascii_case(base))
        {
            candidates.push(base.to_string());
        }
    }
    candidates
}

/// Like [`variant_image_for`] but returns the `device_buttons_image`
/// resource (typically `side_*.png`) — that's the view Logi calibrates
/// the assignment markers against, so the mouse-model render uses it.
pub(super) fn buttons_image_for(
    manifest: &DepotManifest,
    base_model_id: &str,
    ext: u8,
) -> Option<String> {
    manifest
        .resource_for_variant(base_model_id, ext, "device_buttons_image")
        .map(str::to_string)
}

/// Like [`variant_image_for`] but returns the G-series side render
/// ([`SIDE_VIEW_KEY`](super::SIDE_VIEW_KEY)).
pub(super) fn side_image_for(
    manifest: &DepotManifest,
    base_model_id: &str,
    ext: u8,
) -> Option<String> {
    manifest
        .resource_for_variant(base_model_id, ext, super::SIDE_VIEW_KEY)
        .map(str::to_string)
}

/// Like [`variant_image_for`] but returns the `image_metadata` resource —
/// the hotspot-metadata JSON calibrated against this colour variant's
/// renders. Depots whose variants are *handed* rather than coloured ship no
/// well-known metadata name at all (Lift keys its metadata as
/// `core_metadata_left.json` / `core_metadata_right.json`), so the manifest
/// is the only place their filename appears.
pub(super) fn metadata_for(
    manifest: &DepotManifest,
    base_model_id: &str,
    ext: u8,
) -> Option<String> {
    manifest
        .resource_for_variant(base_model_id, ext, "image_metadata")
        .map(str::to_string)
}

/// Load and parse a depot's `manifest.json`, or `None` when it's missing /
/// malformed. Read once per [`load_files`](super::AssetResolver::load_files)
/// so the variant lookups above don't re-parse it for each candidate base.
pub(super) fn load_manifest(dir: &Path) -> Option<DepotManifest> {
    let manifest_path = dir.join("manifest.json");
    if !manifest_path.exists() {
        return None;
    }
    DepotManifest::load_from(&manifest_path)
        .map_err(
            |e| warn!(error = ?e, path = %manifest_path.display(), "depot manifest unreadable"),
        )
        .ok()
}

const MAX_SCALED_CACHE_ENTRIES: usize = 128;
const MAX_DECODED_SOURCES: usize = 64;
pub(crate) const MAX_FALLBACK_ENTRIES: usize = 32;

type ScaledImageEntry = (Arc<RenderImage>, u32, u32);
type ScaledKey = (PathBuf, u32, u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    content_hash: u64,
}

impl FileStamp {
    #[must_use]
    pub fn of(path: &Path) -> Option<Self> {
        use std::hash::{Hash as _, Hasher as _};
        let bytes = std::fs::read(path).ok()?;
        let meta = std::fs::metadata(path).ok()?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        Some(Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            content_hash: hasher.finish(),
        })
    }
}

struct DecodedSource {
    stamp: FileStamp,
    premul_rgba: image::RgbaImage,
}

/// Identity token for an in-flight background image build, binding the request
/// to the cache epoch in which it was created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WorkTicket {
    epoch: u64,
    id: u64,
}

/// Explicit per-key lifecycle state so in-flight work and ready textures are
/// never tracked through independent maps that could disagree.
#[derive(Clone)]
enum SlotState {
    Building {
        ticket: WorkTicket,
        fallback: Option<ScaledImageEntry>,
        prev_ready: Option<(FileStamp, ScaledImageEntry)>,
    },
    Ready {
        stamp: FileStamp,
        image: ScaledImageEntry,
        needs_recheck: bool,
    },
}

/// Result of requesting a scaled render through [`ScaledImageCache::begin_request`].
pub(crate) enum RequestStep {
    /// Fresh cached texture served directly from memory with zero file I/O.
    Ready(ScaledImageEntry),
    /// Build already in flight for this key; draw `display` while waiting.
    InFlight { display: Option<ScaledImageEntry> },
    /// Caller should spawn a background worker for `ticket` and draw `display` meanwhile.
    Spawn {
        ticket: WorkTicket,
        display: Option<ScaledImageEntry>,
    },
}

enum CacheTransition {
    BeginRequest {
        key: ScaledKey,
    },
    MarkForRefresh,
    StoreDecodedSource {
        epoch: u64,
        path: PathBuf,
        source: Arc<DecodedSource>,
    },
    FinishBuild {
        key: ScaledKey,
        ticket: WorkTicket,
        built: Option<(FileStamp, ScaledImageEntry)>,
    },
    Clear,
}

enum TransitionOutcome {
    Step(RequestStep),
    Published(bool),
    Refreshed,
    Cleared,
}

#[derive(Default)]
struct ScaledCacheState {
    epoch: u64,
    next_ticket_id: u64,
    sources: HashMap<PathBuf, Arc<DecodedSource>>,
    slots: HashMap<ScaledKey, SlotState>,
    slot_order: VecDeque<ScaledKey>,
    fallbacks: VecDeque<(PathBuf, ScaledImageEntry)>,
    decode_counts: HashMap<PathBuf, usize>,
}

impl ScaledCacheState {
    fn fallback_for(&self, path: &Path) -> Option<ScaledImageEntry> {
        self.fallbacks
            .iter()
            .rev()
            .find(|(p, _)| p == path)
            .map(|(_, entry)| entry.clone())
    }

    fn record_fallback(&mut self, path: PathBuf, entry: ScaledImageEntry) {
        self.fallbacks.retain(|(p, _)| p != &path);
        if self.fallbacks.len() >= MAX_FALLBACK_ENTRIES {
            self.fallbacks.pop_front();
        }
        self.fallbacks.push_back((path, entry));
    }

    fn insert_slot(&mut self, key: ScaledKey, state: SlotState) {
        if !self.slots.contains_key(&key) {
            if self.slots.len() >= MAX_SCALED_CACHE_ENTRIES
                && let Some(oldest) = self.slot_order.pop_front()
            {
                self.slots.remove(&oldest);
            }
            self.slot_order.push_back(key.clone());
        }
        self.slots.insert(key, state);
    }

    /// Single transition owner for all state mutations in [`ScaledCacheState`].
    fn transition(&mut self, op: CacheTransition) -> TransitionOutcome {
        match op {
            CacheTransition::Clear => {
                self.epoch = self.epoch.wrapping_add(1);
                self.sources.clear();
                self.slots.clear();
                self.slot_order.clear();
                self.fallbacks.clear();
                TransitionOutcome::Cleared
            }
            CacheTransition::MarkForRefresh => {
                self.epoch = self.epoch.wrapping_add(1);
                self.slots.retain(|_, state| {
                    if let SlotState::Ready { needs_recheck, .. } = state {
                        *needs_recheck = true;
                        true
                    } else {
                        false
                    }
                });
                self.slot_order.retain(|k| self.slots.contains_key(k));
                TransitionOutcome::Refreshed
            }
            CacheTransition::BeginRequest { key } => {
                TransitionOutcome::Step(self.begin_request_slot(key))
            }
            CacheTransition::StoreDecodedSource {
                epoch,
                path,
                source,
            } => {
                if epoch != self.epoch {
                    return TransitionOutcome::Published(false);
                }
                if self.sources.len() >= MAX_DECODED_SOURCES && !self.sources.contains_key(&path) {
                    self.sources.clear();
                }
                self.sources.insert(path.clone(), source);
                let count = self.decode_counts.entry(path).or_insert(0);
                *count = count.saturating_add(1);
                TransitionOutcome::Published(true)
            }
            CacheTransition::FinishBuild { key, ticket, built } => {
                TransitionOutcome::Published(self.finish_slot_build(key, ticket, built))
            }
        }
    }

    fn begin_request_slot(&mut self, key: ScaledKey) -> RequestStep {
        if let Some(existing) = self.slots.get(&key).cloned() {
            return match existing {
                SlotState::Ready {
                    stamp,
                    image,
                    needs_recheck,
                } => {
                    if !needs_recheck {
                        return RequestStep::Ready(image);
                    }
                    let ticket = self.issue_ticket();
                    self.slots.insert(
                        key,
                        SlotState::Building {
                            ticket,
                            fallback: Some(image.clone()),
                            prev_ready: Some((stamp, image.clone())),
                        },
                    );
                    RequestStep::Spawn {
                        ticket,
                        display: Some(image),
                    }
                }
                SlotState::Building { fallback, .. } => RequestStep::InFlight { display: fallback },
            };
        }
        let fallback = self.fallback_for(&key.0);
        let ticket = self.issue_ticket();
        self.insert_slot(
            key,
            SlotState::Building {
                ticket,
                fallback: fallback.clone(),
                prev_ready: None,
            },
        );
        RequestStep::Spawn {
            ticket,
            display: fallback,
        }
    }

    fn finish_slot_build(
        &mut self,
        key: ScaledKey,
        ticket: WorkTicket,
        built: Option<(FileStamp, ScaledImageEntry)>,
    ) -> bool {
        if ticket.epoch != self.epoch {
            return false;
        }
        let owns_slot = matches!(
            self.slots.get(&key),
            Some(SlotState::Building {
                ticket: active,
                ..
            }) if *active == ticket
        );
        if !owns_slot {
            return false;
        }
        if let Some((stamp, image)) = built {
            self.record_fallback(key.0.clone(), image.clone());
            self.slots.insert(
                key,
                SlotState::Ready {
                    stamp,
                    image,
                    needs_recheck: false,
                },
            );
            true
        } else {
            self.slots.remove(&key);
            self.slot_order.retain(|k| k != &key);
            false
        }
    }

    fn issue_ticket(&mut self) -> WorkTicket {
        self.next_ticket_id = self.next_ticket_id.wrapping_add(1);
        WorkTicket {
            epoch: self.epoch,
            id: self.next_ticket_id,
        }
    }
}

/// Thread-safe cache of decoded premultiplied-alpha sources and downscaled
/// Lanczos-3 [`RenderImage`] textures.
#[derive(Default)]
pub(crate) struct ScaledImageCache {
    inner: Mutex<ScaledCacheState>,
    #[cfg(test)]
    stamp_reads: AtomicUsize,
}

impl ScaledImageCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            let _ = guard.transition(CacheTransition::Clear);
        }
    }

    /// Mark cached textures so their next request rechecks on-disk [`FileStamp`]
    /// freshness on the background executor after an asset download completes.
    pub fn mark_for_refresh(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            let _ = guard.transition(CacheTransition::MarkForRefresh);
        }
    }

    /// Evaluate the request path for `(path, max_w, max_h)` without performing any
    /// UI-thread file I/O:
    /// - Returns [`RequestStep::Ready`] directly from memory when a verified texture is cached.
    /// - Transitions to `SlotState::Building` with an epoch-bound [`WorkTicket`] on a miss
    ///   or after [`Self::mark_for_refresh`], returning any bounded fallback texture for
    ///   immediate non-blocking drawing while freshness/decode runs in the background.
    #[must_use]
    pub fn begin_request(&self, path: &Path, max_w: u32, max_h: u32) -> RequestStep {
        let key = (path.to_path_buf(), max_w.max(1), max_h.max(1));
        let Ok(mut guard) = self.inner.lock() else {
            return RequestStep::InFlight { display: None };
        };
        match guard.transition(CacheTransition::BeginRequest { key }) {
            TransitionOutcome::Step(step) => step,
            _ => RequestStep::InFlight { display: None },
        }
    }

    /// Execute the decode + Lanczos-3 build for `ticket` and attempt to publish it
    /// through the single transition owner. Old workers from a cleared epoch
    /// or superseded ticket are rejected and return `None`.
    pub fn run_ticket(
        &self,
        path: &Path,
        max_w: u32,
        max_h: u32,
        ticket: WorkTicket,
    ) -> Option<ScaledImageEntry> {
        let max_w = max_w.max(1);
        let max_h = max_h.max(1);
        let key = (path.to_path_buf(), max_w, max_h);
        let built = self.compute_for_epoch(&key, max_w, max_h, ticket);
        let mut guard = self.inner.lock().ok()?;
        let result = built.as_ref().map(|(_, entry)| entry.clone());
        match guard.transition(CacheTransition::FinishBuild { key, ticket, built }) {
            TransitionOutcome::Published(true) => result,
            _ => None,
        }
    }

    fn compute_for_epoch(
        &self,
        key: &ScaledKey,
        max_w: u32,
        max_h: u32,
        ticket: WorkTicket,
    ) -> Option<(FileStamp, ScaledImageEntry)> {
        let path = key.0.as_path();
        #[cfg(test)]
        self.stamp_reads.fetch_add(1, Ordering::Relaxed);
        let stamp = FileStamp::of(path)?;
        let (prev_ready, cached_source) = {
            let guard = self.inner.lock().ok()?;
            if guard.epoch != ticket.epoch {
                return None;
            }
            let prev = match guard.slots.get(key) {
                Some(SlotState::Building {
                    ticket: active,
                    prev_ready,
                    ..
                }) if *active == ticket => prev_ready.clone(),
                _ => None,
            };
            let src = guard
                .sources
                .get(path)
                .filter(|src| src.stamp == stamp)
                .cloned();
            (prev, src)
        };

        if let Some((prev_stamp, prev_image)) = prev_ready
            && prev_stamp == stamp
        {
            return Some((stamp, prev_image));
        }

        let source = if let Some(src) = cached_source {
            src
        } else {
            let decoded = Arc::new(decode_premultiplied_source(path, stamp)?);
            let mut guard = self.inner.lock().ok()?;
            if !matches!(
                guard.transition(CacheTransition::StoreDecodedSource {
                    epoch: ticket.epoch,
                    path: path.to_path_buf(),
                    source: Arc::clone(&decoded),
                }),
                TransitionOutcome::Published(true)
            ) {
                return None;
            }
            decoded
        };

        let built = resize_decoded_source(&source, max_w, max_h)?;
        Some((stamp, built))
    }

    /// Synchronous lookup-or-build helper that drives the same [`begin_request`] +
    /// [`run_ticket`] state machine.
    #[cfg(test)]
    #[must_use]
    pub fn get_or_build(&self, path: &Path, max_w: u32, max_h: u32) -> Option<ScaledImageEntry> {
        match self.begin_request(path, max_w, max_h) {
            RequestStep::Ready(entry) => Some(entry),
            RequestStep::Spawn { ticket, .. } => self.run_ticket(path, max_w, max_h, ticket),
            RequestStep::InFlight { display } => display,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn source_read_count(&self) -> usize {
        self.stamp_reads.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    #[must_use]
    pub fn decode_count(&self, path: &Path) -> usize {
        self.inner
            .lock()
            .ok()
            .and_then(|guard| guard.decode_counts.get(path).copied())
            .unwrap_or(0)
    }

    #[cfg(test)]
    #[must_use]
    pub fn fallback_count(&self) -> usize {
        self.inner.lock().map_or(0, |guard| guard.fallbacks.len())
    }

    #[cfg(test)]
    #[must_use]
    pub fn has_fallback_for(&self, path: &Path) -> bool {
        self.inner
            .lock()
            .is_ok_and(|guard| guard.fallback_for(path).is_some())
    }

    #[cfg(test)]
    #[must_use]
    pub fn is_building_with_ticket(
        &self,
        path: &Path,
        max_w: u32,
        max_h: u32,
        ticket: WorkTicket,
    ) -> bool {
        let key = (path.to_path_buf(), max_w.max(1), max_h.max(1));
        self.inner.lock().is_ok_and(|guard| {
            matches!(
                guard.slots.get(&key),
                Some(SlotState::Building {
                    ticket: active,
                    ..
                }) if *active == ticket
            )
        })
    }
}

static SCALED_IMAGE_CACHE: OnceLock<ScaledImageCache> = OnceLock::new();

fn global_scaled_cache() -> &'static ScaledImageCache {
    SCALED_IMAGE_CACHE.get_or_init(ScaledImageCache::new)
}

/// Evict all cached decoded sources and Lanczos-3 [`RenderImage`] textures when
/// [`super::clear_cache`] wipes the on-disk asset cache.
pub(crate) fn clear_scaled_image_cache() {
    if let Some(cache) = SCALED_IMAGE_CACHE.get() {
        cache.clear();
    }
}

/// Mark cached textures for background [`FileStamp`] revalidation after an
/// asset sync finishes, so drawing [`SlotState::Ready`] hits never touches disk.
pub(crate) fn refresh_scaled_image_cache() {
    if let Some(cache) = SCALED_IMAGE_CACHE.get() {
        cache.mark_for_refresh();
    }
}

/// Contain-fit a box of `aspect` (`width / height`) centered inside `bounds`.
///
/// Shared by [`ScaledImage::fill_stage`] and Home's keyboard `glow_canvas` so
/// a wide RGB keyboard's product render and its inter-key glow always fit the
/// exact same stage rectangle at any card width up to `GALLERY_CARD_MAX_W`.
#[must_use]
pub(crate) fn contain_fit_bounds(bounds: Bounds<Pixels>, aspect: f32) -> Bounds<Pixels> {
    let bw = f32::from(bounds.size.width);
    let bh = f32::from(bounds.size.height);
    if bw <= 0.0 || bh <= 0.0 || aspect <= 0.0 || !aspect.is_finite() {
        return Bounds::default();
    }
    let (rw, rh) = if bw / bh > aspect {
        (bh * aspect, bh)
    } else {
        (bw, bw / aspect)
    };
    let ox = f32::from(bounds.origin.x) + (bw - rw) * 0.5;
    let oy = f32::from(bounds.origin.y) + (bh - rh) * 0.5;
    Bounds {
        origin: point(px(ox), px(oy)),
        size: size(px(rw), px(rh)),
    }
}

#[derive(Clone, Copy)]
enum ScaledFit {
    /// Fit inside the parent stage's runtime layout bounds (`size_full`),
    /// sharing [`contain_fit_bounds`] with `glow_canvas`.
    StageContain,
    /// Fit inside explicit `(max_w, max_h)` logical bounds while preserving aspect ratio.
    BoxContain { max_w: f32, max_h: f32 },
    /// Render into an already aspect-fitted logical box `(width, height)`.
    Exact { width: f32, height: f32 },
}

/// HiDPI-aware GPUI image element that downscales high-resolution asset renders
/// once via premultiplied-alpha Lanczos-3 to match the exact physical pixel box
/// (`logical_size * window.scale_factor()`), eliminating single-pass bilinear
/// aliasing when GPUI lacks texture mipmaps.
#[derive(IntoElement)]
pub(crate) struct ScaledImage {
    path: PathBuf,
    fit: ScaledFit,
}

impl ScaledImage {
    /// Fit `path` dynamically to the full runtime bounds of its parent stage
    /// (`size_full`), sharing [`contain_fit_bounds`] with `glow_canvas`.
    #[must_use]
    pub fn fill_stage(path: PathBuf) -> Self {
        Self {
            path,
            fit: ScaledFit::StageContain,
        }
    }

    /// Fit `path` inside `(max_w, max_h)` logical pixels while preserving aspect ratio.
    #[must_use]
    pub fn contain(path: PathBuf, max_w: f32, max_h: f32) -> Self {
        Self {
            path,
            fit: ScaledFit::BoxContain {
                max_w: max_w.max(1.),
                max_h: max_h.max(1.),
            },
        }
    }

    /// Render `path` into an already aspect-fitted logical box `(width, height)`.
    #[must_use]
    pub fn exact(path: PathBuf, width: f32, height: f32) -> Self {
        Self {
            path,
            fit: ScaledFit::Exact {
                width: width.max(1.),
                height: height.max(1.),
            },
        }
    }
}

impl RenderOnce for ScaledImage {
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "bounded logical and physical pixel dimensions fit within f32/u32"
    )]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        match self.fit {
            ScaledFit::StageContain => {
                let path = self.path;
                canvas(
                    move |bounds, window, cx| {
                        let bw = f32::from(bounds.size.width);
                        let bh = f32::from(bounds.size.height);
                        if bw <= 0.0 || bh <= 0.0 {
                            return None;
                        }
                        let scale_factor = window.scale_factor().clamp(1.0, 4.0);
                        let phys_max_w = (bw * scale_factor).round().max(1.0) as u32;
                        let phys_max_h = (bh * scale_factor).round().max(1.0) as u32;
                        request_scaled_render_image(&path, phys_max_w, phys_max_h, cx)
                    },
                    move |bounds, cached_entry, window, _| {
                        let Some((render_img, dst_w, dst_h)) = cached_entry else {
                            return;
                        };
                        let aspect = (dst_w as f32) / (dst_h.max(1) as f32);
                        let image_bounds = contain_fit_bounds(bounds, aspect);
                        let _ = window.paint_image(
                            bounds,
                            image_bounds,
                            Corners::default(),
                            render_img,
                            0,
                            false,
                        );
                    },
                )
                .size_full()
                .into_any_element()
            }
            ScaledFit::BoxContain { max_w, max_h } => {
                let scale_factor = window.scale_factor().clamp(1.0, 4.0);
                let phys_max_w = (max_w * scale_factor).round().max(1.0) as u32;
                let phys_max_h = (max_h * scale_factor).round().max(1.0) as u32;
                if let Some((render_img, phys_dst_w, phys_dst_h)) =
                    request_scaled_render_image(&self.path, phys_max_w, phys_max_h, cx)
                {
                    let aspect = (phys_dst_w as f32) / (phys_dst_h.max(1) as f32);
                    let (logical_w, logical_h) = if max_w / max_h > aspect {
                        (max_h * aspect, max_h)
                    } else {
                        (max_w, max_w / aspect)
                    };
                    return img(render_img)
                        .w(px(logical_w))
                        .h(px(logical_h))
                        .into_any_element();
                }
                img(self.path).max_w_full().max_h_full().into_any_element()
            }
            ScaledFit::Exact { width, height } => {
                let scale_factor = window.scale_factor().clamp(1.0, 4.0);
                let phys_max_w = (width * scale_factor).round().max(1.0) as u32;
                let phys_max_h = (height * scale_factor).round().max(1.0) as u32;
                if let Some((render_img, _, _)) =
                    request_scaled_render_image(&self.path, phys_max_w, phys_max_h, cx)
                {
                    return img(render_img)
                        .w(px(width))
                        .h(px(height))
                        .into_any_element();
                }
                img(self.path).w(px(width)).h(px(height)).into_any_element()
            }
        }
    }
}

/// Request a scaled [`RenderImage`] without blocking the UI thread:
/// - Returns immediately on an exact `(path, max_w, max_h)` cache hit.
/// - On a cache miss, schedules decoding/Lanczos-3 resizing on GPUI's background
///   executor and returns the most recent cached [`RenderImage`] for `path` (if any)
///   so interactive window resizing stays smooth and non-blocking.
fn request_scaled_render_image(
    path: &Path,
    max_w: u32,
    max_h: u32,
    cx: &mut App,
) -> Option<ScaledImageEntry> {
    let cache = global_scaled_cache();
    match cache.begin_request(path, max_w, max_h) {
        RequestStep::Ready(hit) => Some(hit),
        RequestStep::InFlight { display } => display,
        RequestStep::Spawn { ticket, display } => {
            let bg_path = path.to_path_buf();
            cx.spawn(async move |cx| {
                let published = cx
                    .background_executor()
                    .spawn(async move {
                        global_scaled_cache().run_ticket(&bg_path, max_w, max_h, ticket)
                    })
                    .await;
                if published.is_some() {
                    cx.update(App::refresh_windows);
                }
            })
            .detach();
            display
        }
    }
}

/// Decode `path` once and downscale with premultiplied-alpha Lanczos-3 to fit
/// within `(max_w, max_h)` physical pixels, returning a cached `(RenderImage, dst_w, dst_h)`
/// so GPUI maps 1 texel to 1 physical screen pixel without single-pass bilinear aliasing.
#[cfg(test)]
#[must_use]
pub(crate) fn scaled_render_image(
    path: &Path,
    max_w: u32,
    max_h: u32,
) -> Option<(Arc<RenderImage>, u32, u32)> {
    global_scaled_cache().get_or_build(path, max_w, max_h)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "premultiplied RGBA channel math is bounded within 0..=255"
)]
fn decode_premultiplied_source(path: &Path, stamp: FileStamp) -> Option<DecodedSource> {
    let mut premul_rgba = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?
        .into_rgba8();
    let (src_w, src_h) = premul_rgba.dimensions();
    if src_w == 0 || src_h == 0 {
        return None;
    }

    // Premultiply RGB by alpha once at decode time so transparent (0,0,0,0)
    // exterior pixels never bleed dark fringes into light silhouette edges
    // during subsequent Lanczos-3 resizes.
    for p in premul_rgba.pixels_mut() {
        let a = u16::from(p[3]);
        p[0] = ((u16::from(p[0]) * a + 127) / 255) as u8;
        p[1] = ((u16::from(p[1]) * a + 127) / 255) as u8;
        p[2] = ((u16::from(p[2]) * a + 127) / 255) as u8;
    }

    Some(DecodedSource { stamp, premul_rgba })
}

#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "bounded image dimensions and RGBA channel math fit within f32/u32/u8"
)]
fn resize_decoded_source(
    source: &DecodedSource,
    max_w: u32,
    max_h: u32,
) -> Option<(Arc<RenderImage>, u32, u32)> {
    let (src_w, src_h) = source.premul_rgba.dimensions();
    if src_w == 0 || src_h == 0 {
        return None;
    }

    let scale = (max_w as f32 / src_w as f32)
        .min(max_h as f32 / src_h as f32)
        .min(1.0);
    let dst_w = ((src_w as f32 * scale).round() as u32).max(1);
    let dst_h = ((src_h as f32 * scale).round() as u32).max(1);

    let mut resized = if dst_w == src_w && dst_h == src_h {
        source.premul_rgba.clone()
    } else {
        image::imageops::resize(&source.premul_rgba, dst_w, dst_h, FilterType::Lanczos3)
    };

    // 2. Un-premultiply back to straight alpha.
    for p in resized.pixels_mut() {
        let a = u16::from(p[3]);
        if a > 0 && a < 255 {
            p[0] = ((u16::from(p[0]) * 255 + a / 2) / a).min(255) as u8;
            p[1] = ((u16::from(p[1]) * 255 + a / 2) / a).min(255) as u8;
            p[2] = ((u16::from(p[2]) * 255 + a / 2) / a).min(255) as u8;
        }
    }

    // 3. Apply a mild interior-only unsharp mask on larger renders (e.g. Keys view)
    // so fine key sub-legends (F1..F12) stay crisp without ringing silhouette borders.
    if dst_w >= 200 && dst_h >= 120 {
        sharpen_opaque_interior(&mut resized);
    }

    // 4. GPUI's RenderImage expects BGRA byte order.
    for p in resized.pixels_mut() {
        p.0.swap(0, 2);
    }

    let render_img = Arc::new(RenderImage::new(vec![image::Frame::new(resized)]));
    Some((render_img, dst_w, dst_h))
}

fn sharpen_opaque_interior(img: &mut image::RgbaImage) {
    let (w, h) = img.dimensions();
    if w < 3 || h < 3 {
        return;
    }
    let src = img.clone();
    for y in 1..(h - 1) {
        for x in 1..(w - 1) {
            let center = src.get_pixel(x, y);
            if center[3] < 245 {
                continue;
            }
            let top = src.get_pixel(x, y - 1);
            let bottom = src.get_pixel(x, y + 1);
            let left = src.get_pixel(x - 1, y);
            let right = src.get_pixel(x + 1, y);
            if top[3] < 245 || bottom[3] < 245 || left[3] < 245 || right[3] < 245 {
                continue;
            }
            let out = img.get_pixel_mut(x, y);
            for ch in 0..3 {
                let c = i32::from(center[ch]);
                let avg = (i32::from(top[ch])
                    + i32::from(bottom[ch])
                    + i32::from(left[ch])
                    + i32::from(right[ch]))
                    / 4;
                let diff = c - avg;
                // Mild micro-contrast boost (0.35x) only for subtle legend transitions.
                if diff.abs() <= 48 {
                    out[ch] =
                        u8::try_from((c + (diff * 35) / 100).clamp(0, 255)).unwrap_or(center[ch]);
                }
            }
        }
    }
}
