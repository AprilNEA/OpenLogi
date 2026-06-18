//! Inter-key "hole glow" for a light-up keyboard, painted live from a baked mask.
//!
//! A floating-key keyboard render (e.g. the G513) has many small *enclosed*
//! transparent gaps between its keys. Painting only those holes in the device's
//! lighting colour reads as the keyboard's RGB shining through the gaps — and
//! because holes are interior to the silhouette, the colour can never wrap the
//! outline or bleed into the background.
//!
//! Finding the holes is expensive (a full-image flood-fill), so the assets repo
//! precomputes them once (`scripts/precompute_glow.py`) into each depot's
//! `metadata.json` as a run-length-encoded mask. At runtime we decode that mask
//! into normalized horizontal segments ([`GlowGeometry`]) once per resolve and
//! paint them as scaled, tinted quads on the fly — no pre-rendered PNG and no
//! per-colour texture, so a depot's whole lighting footprint is the segment list.

use std::path::Path;

use serde::Deserialize;
use tracing::warn;

/// Metadata files to read the precomputed mask from, newest schema first.
const META_FILES: [&str; 2] = ["core_metadata.json", "metadata.json"];

/// Sanity bound on a baked mask's stored dimensions. The masks are ~1k px wide;
/// anything far larger is a corrupt or hostile `metadata.json`. The cap also
/// keeps `width * height` well inside `u64`, so the run accumulator can't wrap.
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

/// One horizontal run of inter-key holes, normalized to the mask's `[0, 1]`
/// extent so it scales to whatever size the device image renders at.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GlowSeg {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// The baked inter-key holes as normalized segments plus the mask's aspect
/// ratio, ready to paint over the device image at any size. Decoded once per
/// asset resolve; the segment list is the entire runtime footprint — there is
/// no recoloured texture, so a session that cycles colours costs nothing extra.
#[derive(Debug, Clone)]
pub(crate) struct GlowGeometry {
    pub aspect: f32,
    pub segments: Vec<GlowSeg>,
}

/// Load and decode the precomputed glow mask from a depot directory's metadata.
/// `None` when the depot ships no mask (the feature gate) or it's malformed.
pub(crate) fn load_glow_geometry(dir: &Path) -> Option<GlowGeometry> {
    let mask = META_FILES.iter().find_map(|name| {
        let text = std::fs::read_to_string(dir.join(name)).ok()?;
        serde_json::from_str::<MetaGlow>(&text).ok()?.glow
    })?;
    GlowGeometry::from_mask(&mask)
}

impl GlowGeometry {
    /// Decode the RLE mask into normalized per-row hole segments. A run that
    /// crosses a row boundary is split so every segment stays on one row.
    /// `None` if the stored dimensions are out of range or the runs don't cover
    /// exactly `width * height`.
    #[allow(
        clippy::cast_precision_loss,
        reason = "mask coords are < 8192 px — well within f32 mantissa"
    )]
    fn from_mask(mask: &GlowMask) -> Option<Self> {
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
        let (wf, hf) = (w as f32, h as f32);
        let mut segments = Vec::new();
        let mut idx: u64 = 0;
        let mut on = false;
        for &run in &mask.runs {
            if on && run > 0 {
                let mut start = idx;
                let end = idx + u64::from(run);
                while start < end {
                    let row = start / u64::from(w);
                    let col = start % u64::from(w);
                    let seg_end = end.min((row + 1) * u64::from(w));
                    segments.push(GlowSeg {
                        x: col as f32 / wf,
                        y: row as f32 / hf,
                        w: (seg_end - start) as f32 / wf,
                        h: 1.0 / hf,
                    });
                    start = seg_end;
                }
            }
            idx += u64::from(run);
            on = !on;
        }
        Some(Self {
            aspect: wf / hf,
            segments,
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
mod tests {
    use super::*;

    #[test]
    fn from_mask_extracts_on_runs_as_normalized_segments() {
        // 4x2 mask, runs alternate off/on starting off: off2, on2, off3, on1.
        // Row-major idx 2..4 ON (row 0, cols 2-3); idx 7 ON (row 1, col 3).
        let mask = GlowMask {
            width: 4,
            height: 2,
            runs: vec![2, 2, 3, 1],
        };
        let geom = GlowGeometry::from_mask(&mask).expect("valid mask");
        assert!((geom.aspect - 2.0).abs() < 1e-6);
        assert_eq!(geom.segments.len(), 2);
        let first = geom.segments[0];
        assert!((first.x - 0.5).abs() < 1e-6); // col 2 / 4
        assert!((first.y - 0.0).abs() < 1e-6); // row 0
        assert!((first.w - 0.5).abs() < 1e-6); // len 2 / 4
        let second = geom.segments[1];
        assert!((second.x - 0.75).abs() < 1e-6); // col 3 / 4
        assert!((second.y - 0.5).abs() < 1e-6); // row 1 / 2
    }

    #[test]
    fn from_mask_splits_a_run_across_rows() {
        // 2x2, runs off1 on3: idx 1 (row 0 col 1) + idx 2..4 (row 1) → 2 segments.
        let mask = GlowMask {
            width: 2,
            height: 2,
            runs: vec![1, 3],
        };
        let geom = GlowGeometry::from_mask(&mask).expect("valid mask");
        assert_eq!(geom.segments.len(), 2);
    }

    #[test]
    fn from_mask_rejects_bad_run_total() {
        let mask = GlowMask {
            width: 4,
            height: 4,
            runs: vec![3, 2], // sums to 5, not 16
        };
        assert!(GlowGeometry::from_mask(&mask).is_none());
    }
}
