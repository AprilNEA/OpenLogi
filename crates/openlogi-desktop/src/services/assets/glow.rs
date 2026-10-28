//! Keyboard inter-key glow painted from a baked mask.
//!
//! A floating-key keyboard render (e.g. the G513) has many small *enclosed*
//! transparent gaps between its keys. Painting only those holes in the device's
//! lighting colour reads as the keyboard's RGB shining through the gaps — and
//! because holes are interior to the silhouette, the colour can never wrap the
//! outline or bleed into the background.
//!
//! Finding the holes is expensive (a full-image flood-fill), so the assets
//! pipeline precomputes them once into each depot's `metadata.json` as a
//! run-length-encoded mask. At runtime we decode that mask into normalized
//! horizontal segments ([`GlowGeometry`]) once per resolve and paint them as
//! scaled, tinted quads on the fly — no pre-rendered PNG and no per-colour
//! texture, so a depot's whole lighting footprint is the segment list.

use std::path::Path;

use serde::Deserialize;
use tracing::warn;

/// Metadata files to read the precomputed mask from, newest schema first.
const META_FILES: [&str; 3] = ["core_metadata.json", "metadata_full.json", "metadata.json"];

/// Ceiling on a runtime-derived mask's width — the same ~1k scale as the
/// pipeline-baked masks, and what keeps the flood fill cheap.
const COMPUTED_MASK_MAX_W: u32 = 1024;
/// Alpha below this counts as see-through when deriving holes from a render.
const HOLE_ALPHA: u8 = 96;

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
#[derive(Debug, Clone, Copy, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GlowGeometry {
    pub aspect: f32,
    pub segments: Vec<GlowSeg>,
}

/// Holes for the keyboard render at `image_path`.
///
/// A depot-wide mask in `metadata.json` is one measurement shared by every
/// colour variant. Those renders are not the same picture — the G513 family
/// shifts the keyboard on the canvas between `front.png` and `front_ext*.png`
/// — so painting the shared mask puts the lights off the keys in the grid,
/// the list, and the carousel. When the file is on disk, measure that file.
/// The baked mask is only a fallback when the render itself cannot be read.
pub(crate) fn resolve_glow_geometry(dir: &Path, image_path: &Path) -> Option<GlowGeometry> {
    if image_path.is_file() {
        return compute_glow_geometry(image_path);
    }
    load_glow_geometry(dir)
}

/// Load and decode the precomputed glow mask from a depot directory's metadata.
/// `None` when the depot ships no mask or it's malformed.
fn load_glow_geometry(dir: &Path) -> Option<GlowGeometry> {
    let mask = META_FILES.iter().find_map(|name| {
        let text = std::fs::read_to_string(dir.join(name)).ok()?;
        serde_json::from_str::<MetaGlow>(&text).ok()?.glow
    })?;
    GlowGeometry::from_mask(&mask)
}

/// Derive the inter-key holes straight from the render's alpha channel:
/// flood-fill the see-through field inward from the image border, and whatever
/// see-through cells remain unreached are enclosed by the silhouette — the
/// holes. The image is binned to ≤[`COMPUTED_MASK_MAX_W`] cells wide; a cell
/// is see-through when *any* source pixel in its bin is, so the few-pixel
/// slits between floating keycaps survive the binning. Border-connected
/// transparency (including background reachable through open seams) is
/// "outside" and never glows, which is what keeps the colour inside the
/// silhouette.
fn compute_glow_geometry(image_path: &Path) -> Option<GlowGeometry> {
    let img = image::open(image_path).ok()?.into_rgba8();
    let (src_w, src_h) = img.dimensions();
    if src_w == 0 || src_h == 0 || src_w > MAX_MASK_DIM || src_h > MAX_MASK_DIM {
        return None;
    }
    let scale = src_w.div_ceil(COMPUTED_MASK_MAX_W).max(1);
    let (w, h) = (src_w.div_ceil(scale), src_h.div_ceil(scale));

    // 0 = opaque, 1 = see-through, 2 = see-through and border-connected.
    let mut cells = vec![0u8; (w as usize) * (h as usize)];
    for (x, y, pixel) in img.enumerate_pixels() {
        if pixel.0[3] < HOLE_ALPHA {
            cells[((y / scale) * w + (x / scale)) as usize] = 1;
        }
    }

    let mut queue: std::collections::VecDeque<(u32, u32)> = (0..w)
        .flat_map(|x| [(x, 0), (x, h - 1)])
        .chain((0..h).flat_map(|y| [(0, y), (w - 1, y)]))
        .filter(|&(x, y)| cells[(y * w + x) as usize] == 1)
        .collect();
    for &(x, y) in &queue {
        cells[(y * w + x) as usize] = 2;
    }
    while let Some((x, y)) = queue.pop_front() {
        let neighbors = [
            (x.wrapping_sub(1), y),
            (x + 1, y),
            (x, y.wrapping_sub(1)),
            (x, y + 1),
        ];
        for (nx, ny) in neighbors {
            if nx < w && ny < h && cells[(ny * w + nx) as usize] == 1 {
                cells[(ny * w + nx) as usize] = 2;
                queue.push_back((nx, ny));
            }
        }
    }

    // Normalize to the source image, not the binned grid. The gallery slots
    // (grid card, list thumbnail, carousel card) contain-fit the PNG by its
    // own aspect; a bin-rounded aspect shifts the lights off the keys.
    #[expect(
        clippy::cast_precision_loss,
        reason = "image coords are < 8192 px — well within f32 mantissa"
    )]
    let (image_width, image_height) = (src_w as f32, src_h as f32);
    let mut segments = Vec::new();
    for y in 0..h {
        let mut x = 0;
        while x < w {
            if cells[(y * w + x) as usize] == 1 {
                let start = x;
                while x < w && cells[(y * w + x) as usize] == 1 {
                    x += 1;
                }
                let x0 = start.saturating_mul(scale).min(src_w);
                let x1 = x.saturating_mul(scale).min(src_w);
                let y0 = y.saturating_mul(scale).min(src_h);
                let y1 = y.saturating_add(1).saturating_mul(scale).min(src_h);
                if x1 <= x0 || y1 <= y0 {
                    continue;
                }
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "image coords are < 8192 px — well within f32 mantissa"
                )]
                segments.push(GlowSeg {
                    x: x0 as f32 / image_width,
                    y: y0 as f32 / image_height,
                    w: (x1 - x0) as f32 / image_width,
                    h: (y1 - y0) as f32 / image_height,
                });
            } else {
                x += 1;
            }
        }
    }
    if segments.is_empty() {
        return None;
    }
    Some(GlowGeometry {
        aspect: image_width / image_height,
        segments,
    })
}

impl GlowGeometry {
    /// Decode the RLE mask into normalized per-row hole segments. A run that
    /// crosses a row boundary is split so every segment stays on one row.
    /// `None` if the stored dimensions are out of range or the runs don't cover
    /// exactly `width * height`.
    #[expect(
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

    /// Write an RGBA png where `1` cells are solid and the rest fully
    /// transparent.
    fn write_png(dir: &std::path::Path, name: &str, rows: &[&[u8]]) -> std::path::PathBuf {
        let h = u32::try_from(rows.len()).expect("test image height fits u32");
        let w = u32::try_from(rows[0].len()).expect("test image width fits u32");
        let mut img = image::RgbaImage::new(w, h);
        for (y, row) in rows.iter().enumerate() {
            for (x, &cell) in row.iter().enumerate() {
                let alpha = if cell == 1 { 255 } else { 0 };
                let (x, y) = (
                    u32::try_from(x).expect("test x fits u32"),
                    u32::try_from(y).expect("test y fits u32"),
                );
                img.put_pixel(x, y, image::Rgba([40, 40, 40, alpha]));
            }
        }
        let path = dir.join(name);
        img.save(&path).expect("write png");
        path
    }

    #[test]
    fn computed_geometry_finds_only_enclosed_holes() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A ring of opaque pixels around one transparent pixel (a hole), with
        // border-connected transparency everywhere else.
        let path = write_png(
            dir.path(),
            "ring.png",
            &[
                &[0, 0, 0, 0, 0],
                &[0, 1, 1, 1, 0],
                &[0, 1, 0, 1, 0],
                &[0, 1, 1, 1, 0],
                &[0, 0, 0, 0, 0],
            ],
        );
        let geom = compute_glow_geometry(&path).expect("hole found");
        assert_eq!(geom.segments.len(), 1);
        let seg = geom.segments[0];
        assert!((seg.x - 0.4).abs() < 1e-6, "hole at col 2 of 5");
        assert!((seg.y - 0.4).abs() < 1e-6, "hole at row 2 of 5");
        assert!((geom.aspect - 1.0).abs() < 1e-6);
    }

    #[test]
    fn computed_geometry_ignores_border_connected_transparency() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A C-shape: the notch opens to the border, so nothing is enclosed.
        let path = write_png(
            dir.path(),
            "open.png",
            &[&[1, 1, 1], &[1, 0, 0], &[1, 1, 1]],
        );
        assert!(compute_glow_geometry(&path).is_none());
    }

    #[test]
    fn computed_geometry_skips_fully_opaque_renders() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_png(dir.path(), "solid.png", &[&[1, 1], &[1, 1]]);
        assert!(compute_glow_geometry(&path).is_none());
    }

    /// Colour variants share one `metadata.json` glow. On the G513 that mask
    /// was measured on a different render than the file on screen, so the
    /// lights sit off the keys in every gallery layout. The holes have to
    /// come from the PNG being drawn.
    #[test]
    fn resolve_uses_the_rendered_image_not_a_shifted_depot_mask() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Hole at column 2. A depot mask that lights column 1 instead is the
        // shape of a variant render shifted relative to `metadata.json`.
        let path = write_png(
            dir.path(),
            "front.png",
            &[
                &[0, 0, 0, 0, 0],
                &[0, 1, 1, 1, 0],
                &[0, 1, 0, 1, 0],
                &[0, 1, 1, 1, 0],
                &[0, 0, 0, 0, 0],
            ],
        );
        // 5×5, on only at row 2 col 1 (index 11). Runs start transparent.
        std::fs::write(
            dir.path().join("metadata.json"),
            r#"{"glow":{"width":5,"height":5,"runs":[11,1,13]}}"#,
        )
        .expect("write metadata");

        let geom = resolve_glow_geometry(dir.path(), &path).expect("geometry");
        assert_eq!(geom.segments.len(), 1);
        let seg = geom.segments[0];
        assert!(
            (seg.x - 0.4).abs() < 1e-6,
            "hole follows the rendered image (x={}), not the depot mask",
            seg.x
        );
    }

    /// Two colour variants in one depot do not share a mask. Each file's
    /// holes stay on that file, which is what the grid, list, carousel, and
    /// detail view each draw.
    #[test]
    fn resolve_measures_each_variant_on_its_own_render() {
        let dir = tempfile::tempdir().expect("tempdir");
        let left = write_png(
            dir.path(),
            "front.png",
            &[
                &[0, 0, 0, 0, 0, 0, 0],
                &[0, 1, 1, 1, 1, 1, 0],
                &[0, 1, 0, 1, 1, 1, 0],
                &[0, 1, 1, 1, 1, 1, 0],
                &[0, 0, 0, 0, 0, 0, 0],
            ],
        );
        let right = write_png(
            dir.path(),
            "front_ext5.png",
            &[
                &[0, 0, 0, 0, 0, 0, 0],
                &[0, 1, 1, 1, 1, 1, 0],
                &[0, 1, 1, 1, 0, 1, 0],
                &[0, 1, 1, 1, 1, 1, 0],
                &[0, 0, 0, 0, 0, 0, 0],
            ],
        );
        std::fs::write(
            dir.path().join("metadata.json"),
            r#"{"glow":{"width":7,"height":5,"runs":[35]}}"#,
        )
        .expect("write metadata");

        let left_geom = resolve_glow_geometry(dir.path(), &left).expect("left");
        let right_geom = resolve_glow_geometry(dir.path(), &right).expect("right");
        assert_eq!(left_geom.segments.len(), 1);
        assert_eq!(right_geom.segments.len(), 1);
        assert!((left_geom.segments[0].x - 2.0 / 7.0).abs() < 1e-6);
        assert!((right_geom.segments[0].x - 4.0 / 7.0).abs() < 1e-6);
        assert!((left_geom.aspect - 7.0 / 5.0).abs() < 1e-6);
        assert!((right_geom.aspect - left_geom.aspect).abs() < 1e-6);
    }
}
