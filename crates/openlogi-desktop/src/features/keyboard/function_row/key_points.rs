//! Where the function-row keys sit on the keyboard render.
//!
//! [`key_points`] resolves each key's marker as a fraction of the rendered
//! image: from asset metadata — percent-based markers on MX Keys-class depots,
//! pixel-based ones on legacy keyboard depots (G513) — and, failing both, by
//! even spacing along the F-row.

use super::FUNCTION_KEYS;
use crate::services::assets::ResolvedAsset;

const FALLBACK_KEY_Y_FRAC: f32 = 0.153;
/// Legacy pixel-marker depots (G513 family) mark F1-F12 but not Esc. Esc sits
/// this many key pitches left of F1 on that chassis (measured on the render).
const ESC_LEFT_OF_F1_PITCHES: f32 = 1.55;
/// Logitech key markers are authored against a tighter internal keyboard
/// image. The rendered `front.png` includes a little more top/left padding, so
/// the raw marker lands high-left of the visible keycap center.
const FRONT_MARKER_X_OFFSET_FRAC: f32 = 0.02;
const FRONT_MARKER_Y_OFFSET_FRAC: f32 = 0.023;
/// Even-spacing fallback band (fractions of image width) when no metadata.
pub(super) const EVEN_SPACING_START: f32 = 0.04;
pub(super) const EVEN_SPACING_END: f32 = 0.96;

#[derive(Clone, Copy, Debug)]
pub(super) struct KeyPoint {
    pub(super) x_frac: f32,
    pub(super) y_frac: f32,
}

/// Resolve key marker points as fractions [0..1] of the rendered image, along
/// with how many top-row keys the board exposes (`points.len()` — the visible
/// prefix of [`FUNCTION_KEYS`]). Prefer asset metadata's top-row markers —
/// percent-based on MX Keys-class depots, pixel-based on legacy keyboard
/// depots (G513) — and fall back to even spacing on the same row.
pub(super) fn key_points(asset: Option<&ResolvedAsset>) -> Vec<KeyPoint> {
    if let Some(a) = asset {
        if let Some(points) = legacy_pixel_key_points(a) {
            return points;
        }
        let key_markers = sorted_marker_points(a, &["device_keys_image", "device_buttons_image"]);
        let easy_switch_markers = sorted_marker_points(a, &["device_easyswitch_image"]);

        if key_markers.len() >= 16 && easy_switch_markers.len() >= 3 {
            let mut out = Vec::with_capacity(FUNCTION_KEYS.len());
            out.push(synthesized_esc_point(key_markers[0]));
            out.extend(
                key_markers[..12]
                    .iter()
                    .copied()
                    .map(calibrated_marker_point),
            );
            out.extend(
                easy_switch_markers[..3]
                    .iter()
                    .copied()
                    .map(calibrated_marker_point),
            );
            out.extend(
                key_markers[key_markers.len() - 4..]
                    .iter()
                    .copied()
                    .map(calibrated_marker_point),
            );
            if out.len() == FUNCTION_KEYS.len() {
                return out;
            }
        }

        if key_markers.len() >= FUNCTION_KEYS.len() - 1 {
            let f1_to_f19 = &key_markers[..FUNCTION_KEYS.len() - 1];
            let mut out = Vec::with_capacity(FUNCTION_KEYS.len());
            out.push(synthesized_esc_point(f1_to_f19[0]));
            out.extend(f1_to_f19.iter().copied().map(calibrated_marker_point));
            return out;
        }
    }
    fallback_key_points()
}

#[cfg(test)]
pub(super) fn key_x_fractions(asset: Option<&ResolvedAsset>) -> Vec<f32> {
    key_points(asset)
        .into_iter()
        .map(|point| point.x_frac)
        .collect()
}

/// Key points from a legacy pixel-marker depot (the G513 family), or `None`
/// when the asset isn't one.
///
/// Legacy `metadata*.json` files mark each F-key's cap-face centre in
/// *absolute pixels* of the authored canvas (`origin`), not percentages. The
/// markers only apply when that canvas is the render we actually cached —
/// the same depot also ships marker sets authored against other variants'
/// renders (the G513's `metadata.json` belongs to the G512 banner render) —
/// so a depot whose `origin` doesn't match the PNG is rejected rather than
/// misplacing every callout.
fn legacy_pixel_key_points(asset: &ResolvedAsset) -> Option<Vec<KeyPoint>> {
    let img = asset
        .metadata
        .images
        .iter()
        .find(|img| img.key == "device_image" && !img.assignments.is_empty())?;
    if img.origin.width != asset.png_width || img.origin.height != asset.png_height {
        return None;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "depot image dimensions are a few thousand pixels at most"
    )]
    let (w, h) = (img.origin.width as f32, img.origin.height as f32);

    let mut markers: Vec<KeyPoint> = img
        .assignments
        .iter()
        .map(|asg| asg.marker)
        // Percent-schema depots never exceed 100 on either axis; anything
        // beyond is a pixel coordinate. Mixed files don't exist in the wild,
        // but a percent marker slipping through would land off by 27x.
        .filter(|m| m.x > 100. || m.y > 100.)
        .map(|m| KeyPoint {
            x_frac: (m.x / w).clamp(0.0, 1.0),
            y_frac: (m.y / h).clamp(0.0, 1.0),
        })
        .collect();
    if markers.len() < 2 || markers.len() > FUNCTION_KEYS.len() - 1 {
        return None;
    }
    markers.sort_by(|a, b| {
        a.x_frac
            .partial_cmp(&b.x_frac)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // The depots mark F1..Fn but never Esc; place it left of F1 by the F-row's
    // own key pitch so it stays registered at any render size.
    let pitch = median_pitch(&markers)?;
    let first = markers[0];
    let esc = KeyPoint {
        x_frac: (first.x_frac - ESC_LEFT_OF_F1_PITCHES * pitch).max(0.0),
        y_frac: first.y_frac,
    };

    let mut out = Vec::with_capacity(markers.len() + 1);
    out.push(esc);
    out.extend(markers);
    Some(out)
}

/// Median gap between adjacent marker x positions — the F-row's key pitch.
/// The median rides out the wider inter-cluster gaps (F4→F5, F8→F9).
fn median_pitch(sorted_markers: &[KeyPoint]) -> Option<f32> {
    let mut gaps: Vec<f32> = sorted_markers
        .windows(2)
        .map(|pair| pair[1].x_frac - pair[0].x_frac)
        .filter(|gap| *gap > 0.)
        .collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_by(f32::total_cmp);
    Some(gaps[gaps.len() / 2])
}

fn sorted_marker_points(asset: &ResolvedAsset, image_keys: &[&str]) -> Vec<KeyPoint> {
    let mut markers: Vec<KeyPoint> = asset
        .metadata
        .images
        .iter()
        .filter(|img| image_keys.contains(&img.key.as_str()))
        .flat_map(|img| img.assignments.iter())
        .map(|asg| KeyPoint {
            x_frac: asg.marker.x / 100.0,
            y_frac: asg.marker.y / 100.0,
        })
        .collect();
    markers.sort_by(|a, b| {
        a.x_frac
            .partial_cmp(&b.x_frac)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    markers
}

fn synthesized_esc_point(first_function_key: KeyPoint) -> KeyPoint {
    KeyPoint {
        x_frac: synthesized_esc_x(first_function_key.x_frac),
        y_frac: calibrated_marker_point(first_function_key).y_frac,
    }
}

fn calibrated_marker_point(raw: KeyPoint) -> KeyPoint {
    KeyPoint {
        x_frac: (raw.x_frac + FRONT_MARKER_X_OFFSET_FRAC).clamp(0.0, 1.0),
        y_frac: (raw.y_frac + FRONT_MARKER_Y_OFFSET_FRAC).clamp(0.0, 1.0),
    }
}

fn synthesized_esc_x(first_function_key_x: f32) -> f32 {
    (first_function_key_x - 0.045).max(0.02)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "FUNCTION_KEYS is a fixed table of a dozen entries"
)]
fn fallback_key_x_fractions() -> Vec<f32> {
    let step = (EVEN_SPACING_END - EVEN_SPACING_START) / (FUNCTION_KEYS.len() - 1) as f32;
    (0..FUNCTION_KEYS.len())
        .map(|i| EVEN_SPACING_START + (i as f32) * step)
        .collect()
}

fn fallback_key_points() -> Vec<KeyPoint> {
    fallback_key_x_fractions()
        .into_iter()
        .map(|x_frac| KeyPoint {
            x_frac,
            y_frac: FALLBACK_KEY_Y_FRAC,
        })
        .collect()
}
