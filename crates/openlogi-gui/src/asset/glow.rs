//! Inter-key "hole glow" overlay for a light-up keyboard.
//!
//! A floating-key keyboard render (e.g. the G513) has many small *enclosed*
//! transparent gaps between its keys. Painting only those holes in the device's
//! lighting colour reads as the keyboard's RGB shining through the gaps — and
//! because holes are interior to the silhouette, the colour can never wrap the
//! outline or bleed into the background.
//!
//! Finding the holes is expensive (a full-image flood-fill), so the assets repo
//! precomputes them once (`scripts/precompute_glow.py`) into each depot's
//! `metadata.json` as a run-length-encoded mask. At runtime we only recolour
//! that tiny mask per lighting colour — no flood-fill, no full-image decode.
//! A depot without a precomputed mask simply gets no overlay.

use std::path::PathBuf;

use image::{Rgba, RgbaImage};
use serde::Deserialize;
use tracing::{debug, warn};

use crate::components::lighting_panel::parse_hex;

/// Metadata files to read the precomputed mask from, newest schema first.
const META_FILES: [&str; 2] = ["core_metadata.json", "metadata.json"];

/// Sanity bound on a baked mask's stored dimensions. The masks are ~1k px wide;
/// anything far larger is a corrupt or hostile `metadata.json`. The cap also
/// keeps `width * height` well inside `u32`, so the run accumulator can't wrap.
const MAX_MASK_DIM: u32 = 8192;

/// Precomputed inter-key hole mask embedded in a depot's `metadata.json`:
/// a run-length-encoded binary mask, row-major, runs alternating
/// transparent/opaque starting transparent (so `sum(runs) == width * height`).
#[derive(Deserialize)]
struct GlowMask {
    width: u32,
    height: u32,
    runs: Vec<u32>,
}

#[derive(Deserialize)]
struct MetaGlow {
    #[serde(default)]
    glow: Option<GlowMask>,
}

/// Generate (once, then cache) a `glow-<hex>.png` overlay for a keyboard: the
/// precomputed inter-key holes painted `hex`, transparent elsewhere. `None`
/// unless the depot ships a precomputed mask (the feature gate) or the cache
/// can't be written. Cached under the writable user dir keyed by `depot`, so it
/// survives a read-only `.app` bundle.
pub(crate) fn ensure_glow_png(depot: &str, hex: &str) -> Option<PathBuf> {
    let dir = depot_dir(depot)?;
    let out = dir.join(format!("glow-{hex}.png"));
    if out.exists() {
        return Some(out);
    }
    let [_, r, g, b] = parse_hex(hex).to_be_bytes();
    let color = Rgba([r, g, b, 255]);
    let overlay = render_mask(&read_baked_mask(depot)?, color)?;

    std::fs::create_dir_all(&dir).ok()?;
    // Write atomically (temp + rename) so a concurrent render never loads a
    // half-written PNG; gpui caches an image-load *failure* permanently. For the
    // same reason we keep every colour variant on disk (bounded by the small
    // swatch palette) — deleting one would strand the GUI's in-memory "ready"
    // set pointing at a file that no longer exists, blanking the card.
    let tmp = dir.join(format!("glow-{hex}.png.tmp"));
    overlay
        .save_with_format(&tmp, image::ImageFormat::Png)
        .map_err(|e| warn!(path = %tmp.display(), error = %e, "glow: save failed"))
        .ok()?;
    std::fs::rename(&tmp, &out).ok()?;
    debug!(depot, hex, "glow: cached");
    Some(out)
}

/// Cache path for a depot's glow overlay at colour `hex` (stat-only — no
/// writes). `None` when the depot name isn't a safe single path component,
/// so read-side lookups stay inside the cache root just like the writers.
pub(crate) fn glow_path(depot: &str, hex: &str) -> Option<PathBuf> {
    Some(depot_dir(depot)?.join(format!("glow-{hex}.png")))
}

/// Validated `<user_cache_root>/<depot>` — `None` (with a warn) when the
/// index-supplied depot name isn't a single safe path component, so glow
/// IO can never leave the cache root.
fn depot_dir(depot: &str) -> Option<PathBuf> {
    openlogi_assets::http::safe_component_path(
        &super::paths::user_cache_root(),
        depot,
        "asset depot",
    )
    .map_err(|e| warn!(depot, error = %e, "glow: refusing depot dir"))
    .ok()
}

/// Read the precomputed `glow` mask from the depot's metadata, ignoring every
/// other field (so the keyboard-schema hotspot data is irrelevant here).
fn read_baked_mask(depot: &str) -> Option<GlowMask> {
    let dir = depot_dir(depot)?;
    META_FILES.iter().find_map(|name| {
        let text = std::fs::read_to_string(dir.join(name)).ok()?;
        serde_json::from_str::<MetaGlow>(&text).ok()?.glow
    })
}

/// Paint the RLE mask in `color`, then soften.
fn render_mask(mask: &GlowMask, color: Rgba<u8>) -> Option<RgbaImage> {
    Some(image::imageops::blur(&paint_mask(mask, color)?, 1.5))
}

/// Reconstruct the RLE mask into a transparent image with the on-runs painted
/// `color`. `None` if the runs don't cover exactly `width * height`.
fn paint_mask(mask: &GlowMask, color: Rgba<u8>) -> Option<RgbaImage> {
    let (w, h) = (mask.width, mask.height);
    if w == 0 || h == 0 || w > MAX_MASK_DIM || h > MAX_MASK_DIM {
        warn!(w, h, "glow: precomputed mask dimensions out of range");
        return None;
    }
    let total = u64::from(w) * u64::from(h);
    if mask.runs.iter().map(|&r| u64::from(r)).sum::<u64>() != total {
        warn!(w, h, "glow: precomputed mask runs don't cover width*height");
        return None;
    }
    let mut img = RgbaImage::new(w, h);
    let mut idx: u32 = 0;
    let mut on = false;
    for &run in &mask.runs {
        if on {
            for p in idx..idx + run {
                img.put_pixel(p % w, p / w, color);
            }
        }
        idx += run;
        on = !on;
    }
    Some(img)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
mod tests {
    use super::*;

    #[test]
    fn paint_mask_paints_only_on_runs() {
        // 3x2 mask, runs alternate off/on starting off: off2, on1, off2, on1.
        // Row-major pixels idx 2 and idx 5 are ON.
        let mask = GlowMask {
            width: 3,
            height: 2,
            runs: vec![2, 1, 2, 1],
        };
        let img = paint_mask(&mask, Rgba([10, 20, 30, 255])).expect("mask paints");
        assert_eq!(img.get_pixel(2, 0).0, [10, 20, 30, 255]); // idx 2, on
        assert_eq!(img.get_pixel(2, 1).0, [10, 20, 30, 255]); // idx 5, on
        assert_eq!(img.get_pixel(0, 0).0[3], 0); // idx 0, off → transparent
    }

    #[test]
    fn paint_mask_rejects_bad_run_total() {
        let mask = GlowMask {
            width: 4,
            height: 4,
            runs: vec![3, 2], // sums to 5, not 16
        };
        assert!(paint_mask(&mask, Rgba([0, 0, 0, 255])).is_none());
    }
}
