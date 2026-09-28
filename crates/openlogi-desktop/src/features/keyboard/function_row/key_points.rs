//! Which keys the Keys tab shows, and where they sit on the keyboard render.
//!
//! [`key_slots`] answers both from one source. A Logi depot with
//! `device_keys_image` markers names every remappable key by its HID++
//! `0x1b04` control ID (the `_c<decimal>` suffix of the slot ID), so those
//! keys are shown as [`KeyTarget::Control`] at their marked position and bind
//! through the device's own HID++ diversion — the only path that reaches a
//! media or shortcut key, which the OS hook never sees. A depot without such
//! markers (the G513 family's pixel-marker files, or no depot at all) falls
//! back to the OS-hook F-row: Esc and F1–F19 as [`KeyTarget::FunctionKey`],
//! placed by the legacy pixel markers or by even spacing.

use openlogi_core::binding::ButtonId;
use openlogi_core::config::{FunctionKey, KeyModifiers, KeyTrigger};

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

/// The metadata image entries that carry remappable-key markers. The
/// Easy-Switch host keys live under their own entry and are deliberately not
/// read: the host-switch session owns them.
const KEY_MARKER_IMAGES: [&str; 2] = ["device_keys_image", "device_buttons_image"];

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct KeyPoint {
    pub(super) x_frac: f32,
    pub(super) y_frac: f32,
}

/// What a key on the Keys tab binds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeyTarget {
    /// A HID++ control of the selected keyboard, bound per device and
    /// diverted by the agent while bound.
    Control(ButtonId),
    /// An OS-hook F-row trigger, bound globally across keyboards.
    FunctionKey(KeyTrigger),
}

impl KeyTarget {
    /// The key's legend: a control's localized catalog name, or the F-row
    /// trigger as it is written in `config.toml`.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Control(button) => control_legend(*button),
            Self::FunctionKey(trigger) => trigger.to_string(),
        }
    }
}

/// A control's localized name; one OpenLogi has no catalog row for is the
/// generic word plus its number, `Control 0x01f3`.
fn control_legend(button: ButtonId) -> String {
    let name = rust_i18n::t!(button.translation_key());
    match button.cid().filter(|cid| cid.known().is_none()) {
        Some(cid) => format!("{name} {}", cid.hex()),
        None => name.into_owned(),
    }
}

/// One key the tab shows: its target, legend, and marker position as
/// fractions of the rendered image.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct KeySlotLayout {
    pub(super) target: KeyTarget,
    pub(super) legend: String,
    pub(super) point: KeyPoint,
}

/// The keys `asset` lets the tab show, in physical left-to-right order for a
/// marker-based board (top row first) or F-row order for the fallback.
pub(super) fn key_slots(asset: Option<&ResolvedAsset>) -> Vec<KeySlotLayout> {
    if let Some(asset) = asset {
        let controls = control_slots(asset);
        if !controls.is_empty() {
            return controls;
        }
        if let Some(points) = legacy_pixel_key_points(asset) {
            return function_key_slots(points);
        }
    }
    function_key_slots(fallback_key_points())
}

/// The depot's control markers as key slots, each named by the control's
/// catalog label (or its number). Ordered top row first, then left to right,
/// so the callout band reads the way the keyboard does; the keys above the
/// numpad and down the right edge follow the F-row.
fn control_slots(asset: &ResolvedAsset) -> Vec<KeySlotLayout> {
    // Rows are banded by y; within a band, left to right. Markers on one
    // physical row differ by well under a band.
    const ROW_BAND: f32 = 0.06;
    let mut slots: Vec<KeySlotLayout> = asset
        .metadata
        .images
        .iter()
        .filter(|img| KEY_MARKER_IMAGES.contains(&img.key.as_str()))
        .flat_map(|img| img.assignments.iter())
        .filter_map(|assignment| {
            let button = ButtonId::control(assignment.control_id()?);
            Some(KeySlotLayout {
                legend: control_legend(button),
                target: KeyTarget::Control(button),
                point: calibrated_marker_point(KeyPoint {
                    x_frac: assignment.marker.x / 100.0,
                    y_frac: assignment.marker.y / 100.0,
                }),
            })
        })
        .collect();
    slots.sort_by(|a, b| {
        let row_a = (a.point.y_frac / ROW_BAND).floor();
        let row_b = (b.point.y_frac / ROW_BAND).floor();
        row_a
            .total_cmp(&row_b)
            .then_with(|| a.point.x_frac.total_cmp(&b.point.x_frac))
    });
    // The same control can be marked twice (a `device_buttons_image` copy);
    // one slot per control.
    slots.dedup_by(|a, b| a.target == b.target);
    slots
}

/// Esc and F1–Fn over `points`, one per point.
fn function_key_slots(points: Vec<KeyPoint>) -> Vec<KeySlotLayout> {
    FunctionKey::ALL
        .into_iter()
        .zip(points)
        .map(|(key, point)| KeySlotLayout {
            target: KeyTarget::FunctionKey(KeyTrigger {
                keycode: key.keycode(),
                modifiers: KeyModifiers::default(),
            }),
            legend: key.label().to_owned(),
            point,
        })
        .collect()
}

#[cfg(test)]
pub(super) fn key_x_fractions(asset: Option<&ResolvedAsset>) -> Vec<f32> {
    key_slots(asset)
        .into_iter()
        .map(|slot| slot.point.x_frac)
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
    if markers.len() < 2 || markers.len() > FunctionKey::ALL.len() - 1 {
        return None;
    }
    markers.sort_by(|a, b| a.x_frac.total_cmp(&b.x_frac));

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

fn calibrated_marker_point(raw: KeyPoint) -> KeyPoint {
    KeyPoint {
        x_frac: (raw.x_frac + FRONT_MARKER_X_OFFSET_FRAC).clamp(0.0, 1.0),
        y_frac: (raw.y_frac + FRONT_MARKER_Y_OFFSET_FRAC).clamp(0.0, 1.0),
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "FunctionKey::ALL is a fixed table of a dozen entries"
)]
fn fallback_key_x_fractions() -> Vec<f32> {
    let count = FunctionKey::ALL.len();
    let step = (EVEN_SPACING_END - EVEN_SPACING_START) / (count - 1) as f32;
    (0..count)
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
