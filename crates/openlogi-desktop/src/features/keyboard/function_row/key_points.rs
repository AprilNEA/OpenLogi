//! Which keys the Keys tab shows, and where they sit on the keyboard render.
//!
//! [`key_surface`] answers both from one source. A Logi depot with
//! `device_keys_image` markers names every remappable key by its HID++
//! `0x1b04` control ID (the `_c<decimal>` suffix of the slot ID), so those
//! keys are shown as [`KeyTarget::Control`] at their marked position and bind
//! through the device's own HID++ diversion — the only path that reaches a
//! media or shortcut key, which the OS hook never sees. A keyboard depot
//! without such markers (the G513 family's pixel-marker files, or no depot
//! at all) falls back to the OS-hook F-row: Esc and F1–F19 as
//! [`KeyTarget::FunctionKey`], placed by the legacy pixel markers or by even
//! spacing. A keypad is never that keyboard — it carries no F-row at all.
//! Where its depot marks the grid keys — by the `_g<index>` suffix, since
//! the vendor report that carries their presses names no control ID — the
//! keys sit on the device render. Logi's own keypad depot marks none of
//! them, so for the stock MX Creative Keypad render OpenLogi carries the key
//! centres measured off that very render ([`stock_keypad_slots`]) and maps
//! the keys onto it the same way. Otherwise the known keypad shows its own
//! keys, the LCD grid keys and paging buttons, as the keycap layout the
//! device model describes ([`KeyTarget::Grid`]). Either way every key binds
//! per device like any control. Those keys are the keypad's identity fact
//! ([`DeviceRoute::keypad`](openlogi_core::hid::DeviceRoute::keypad)): a
//! number pad that is not that keypad has none of them, and is drawn from
//! its own markers like a keyboard.

use std::collections::HashSet;

use openlogi_core::binding::{ButtonId, Cid, GridKey, GridSlot, KeypadPage};
use openlogi_core::config::{FunctionKey, KeyModifiers, KeyTrigger};
use openlogi_core::hid::KeypadDescriptor;

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
    /// A keypad grid key on one binding page, bound per device like a control
    /// but identified by its position: the vendor report that carries its
    /// presses names no control. Layouts place page-1 slots; the view moves
    /// them to the page it is editing ([`Self::on_page`]).
    Grid(GridSlot),
    /// An OS-hook F-row trigger, bound globally across keyboards.
    FunctionKey(KeyTrigger),
}

impl KeyTarget {
    /// The first-page target for grid key `key`.
    pub(crate) const fn grid(key: GridKey) -> Self {
        Self::Grid(GridSlot {
            page: KeypadPage::FIRST,
            key,
        })
    }

    /// This target as the view edits it on `page`: a grid key moves to that
    /// page's slot; every other target has no pages and stays as it is.
    #[must_use]
    pub(crate) fn on_page(self, page: KeypadPage) -> Self {
        match self {
            Self::Grid(slot) => Self::Grid(GridSlot { page, ..slot }),
            other => other,
        }
    }

    /// The key's legend: a control's or grid key's localized catalog name,
    /// or the F-row trigger as it is written in `config.toml`.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Control(button) => control_legend(*button),
            Self::Grid(slot) => rust_i18n::t!(slot.key.translation_key()).into_owned(),
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

/// The keys the device presents, and the surface that draws them.
pub(super) enum KeySurface {
    /// The device's photo with its keys placed on it: a keyboard's callout
    /// bubbles and leader lines, or a keypad's keys drawn on the render —
    /// the markers (or the stock keypad's measured centres) name the keys;
    /// without them a keyboard's slots land on the evenly spaced F-row band.
    Photo(Vec<KeySlotLayout>),
    /// A keypad's own keycap map: every key binds this device, the
    /// positions come from the device model, and no photo participates.
    Keypad(Vec<KeySlotLayout>),
}

impl KeySurface {
    pub(super) fn slots(self) -> Vec<KeySlotLayout> {
        match self {
            Self::Photo(slots) | Self::Keypad(slots) => slots,
        }
    }
}

/// Which keys the tab shows, and where. A depot with control markers is
/// the keyboard's own answer (photo path); the known `keypad` goes on
/// its render when the depot marks its grid or the render is the stock one
/// OpenLogi measured, else onto the device-model keycap map; any other
/// markerless device falls back to the OS-hook F-row, which is genuinely
/// that device's remappable surface only because a full keyboard really has
/// that row.
pub(super) fn key_surface(
    asset: Option<&ResolvedAsset>,
    keypad: Option<&KeypadDescriptor>,
) -> KeySurface {
    if let Some(asset) = asset {
        let marked = marker_slots(asset, keypad);
        if !marked.is_empty() {
            // A marked keypad goes onto the photo only once the depot marks
            // the grid too: paging-only markers would show two bubbles and
            // hide the nine keys the tab exists to bind, so until then the
            // device-model keycap map — which shows every key, always — is
            // the honest surface.
            let marks_grid = marked
                .iter()
                .any(|slot| matches!(slot.target, KeyTarget::Grid(_)));
            if marks_grid || keypad.is_none() {
                return KeySurface::Photo(marked);
            }
        }
        if keypad.is_some() {
            return stock_keypad_slots(asset)
                .map_or_else(|| KeySurface::Keypad(keypad_slots()), KeySurface::Photo);
        }
        if let Some(points) = legacy_pixel_key_points(asset) {
            return KeySurface::Photo(function_key_slots(points));
        }
    } else if keypad.is_some() {
        return KeySurface::Keypad(keypad_slots());
    }
    KeySurface::Photo(function_key_slots(fallback_key_points()))
}

/// The slots `key_surface` drew — a test convenience; render consumes the
/// surface itself to pick the pane.
#[cfg(test)]
pub(super) fn key_slots(
    asset: Option<&ResolvedAsset>,
    keypad: Option<&KeypadDescriptor>,
) -> Vec<KeySlotLayout> {
    key_surface(asset, keypad).slots()
}

/// Schematic keycap geometry, in CSS px. `function_row`'s `KeypadPane`
/// sizes its canvas and its caps from these same constants.
pub(super) const KEYCAP_W: f32 = 96.;
pub(super) const KEYCAP_H: f32 = 64.;
/// The paging keys read as lower caps under the grid.
pub(super) const KEYCAP_PAGING_H: f32 = 40.;
pub(super) const KEYCAP_GAP: f32 = 12.;

/// Whether `cid` marks one of the keypad's paging keys ([`Cid::PAGING`],
/// printed `‹` and `›`). A marked keypad's depot
/// may address them beside the grid like any other control; the schematic
/// draws them as the lower caps they physically are.
#[must_use]
pub(super) fn is_paging_cid(cid: Cid) -> bool {
    Cid::PAGING.contains(&cid)
}

/// The canvas keycap points are fractions of: three columns, three grid
/// rows, and a paging row, one gap between every cell.
pub(super) fn keypad_canvas() -> (f32, f32) {
    (
        3.0 * (KEYCAP_W + KEYCAP_GAP) - KEYCAP_GAP,
        3.0 * (KEYCAP_H + KEYCAP_GAP) + KEYCAP_GAP + KEYCAP_PAGING_H,
    )
}

/// The centre of cell `(col, row)` on the keycap canvas.
#[expect(
    clippy::cast_precision_loss,
    reason = "keypad cell indices are single digits"
)]
fn keycap_point(col: usize, row: usize) -> KeyPoint {
    let (w, h) = keypad_canvas();
    KeyPoint {
        x_frac: ((col as f32) * (KEYCAP_W + KEYCAP_GAP) + KEYCAP_W / 2.0) / w,
        y_frac: ((row as f32) * (KEYCAP_H + KEYCAP_GAP) + KEYCAP_H / 2.0) / h,
    }
}

/// The keypad's keys in physical reading order: the nine LCD keys
/// row-major, then left and right paging under the first two grid columns
/// as they sit on the chassis. Their identities are the ones OpenLogi
/// binds by — grid positions, and the two controls catalogued for the
/// keypad's paging feature — placed by `grid_point(row, col)` and
/// `paging_point(col)`.
fn keypad_layout(
    grid_point: impl Fn(usize, usize) -> KeyPoint,
    paging_point: impl Fn(usize) -> KeyPoint,
) -> Vec<KeySlotLayout> {
    let mut slots: Vec<KeySlotLayout> = GridKey::ALL
        .iter()
        .map(|key| {
            let position = key.position();
            KeySlotLayout {
                legend: rust_i18n::t!(key.translation_key()).into_owned(),
                target: KeyTarget::grid(*key),
                point: grid_point(usize::from(position.row), usize::from(position.col)),
            }
        })
        .collect();
    for (col, cid) in Cid::PAGING.into_iter().enumerate() {
        let button = ButtonId::Control(cid);
        slots.push(KeySlotLayout {
            legend: control_legend(button),
            target: KeyTarget::Control(button),
            point: paging_point(col),
        });
    }
    slots
}

/// The keypad's keys on the schematic keycap canvas.
#[expect(
    clippy::cast_precision_loss,
    reason = "keypad cell indices are single digits"
)]
fn keypad_slots() -> Vec<KeySlotLayout> {
    let (w, h) = keypad_canvas();
    keypad_layout(
        |row, col| keycap_point(col, row),
        |col| KeyPoint {
            x_frac: ((col as f32) * (KEYCAP_W + KEYCAP_GAP) + KEYCAP_W / 2.0) / w,
            y_frac: (3.0 * (KEYCAP_H + KEYCAP_GAP) + KEYCAP_GAP + KEYCAP_PAGING_H / 2.0) / h,
        },
    )
}

/// The depot whose stock render [`stock_keypad_slots`] was measured on.
const STOCK_KEYPAD_DEPOT: &str = "mx_creative_keypad";
/// That render's size, `front_core.png`. A re-staged render of any other
/// size is no longer the one measured, so the keys fall back to the
/// schematic rather than landing off the keys.
const STOCK_KEYPAD_RENDER: (u32, u32) = (935, 1105);
/// Key centres on the stock render, in its pixels: the LCD grid's columns
/// and rows, and the paging keys' row and columns.
const STOCK_GRID_COLS_PX: [f32; 3] = [233., 471., 709.];
const STOCK_GRID_ROWS_PX: [f32; 3] = [229., 467.5, 706.];
const STOCK_PAGING_COLS_PX: [f32; 2] = [233., 473.];
const STOCK_PAGING_ROW_PX: f32 = 906.5;

/// A key's outline on the keypad render, in px of the render as drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct KeyShape {
    pub(super) w: f32,
    pub(super) h: f32,
    /// Corner radii: top left, top right, bottom right, bottom left.
    pub(super) radii: [f32; 4],
}

/// Key outlines on the stock render, in its pixels. Most corners are the
/// keycaps' small radius; the chassis' own curve gives the grid's outer top
/// corners and the paging keys' outer bottom corners a wide one.
const GRID_KEY_PX: f32 = 200.;
const PAGING_KEY_PX: (f32, f32) = (200., 127.);
const KEY_CORNER_PX: f32 = 22.;
const KEY_OUTER_CORNER_PX: f32 = 60.;
const PAGING_TOP_CORNER_PX: f32 = 18.;
const PAGING_INNER_CORNER_PX: f32 = 24.;
const PAGING_OUTER_CORNER_PX: f32 = 55.;

/// The outline of the key `target` names on a keypad render `render_w` px
/// wide — the stock render's measured key, scaled to the drawn render.
#[expect(
    clippy::cast_precision_loss,
    reason = "render dimensions are a few thousand pixels at most"
)]
pub(super) fn photo_key_shape(target: &KeyTarget, render_w: f32) -> KeyShape {
    let (k, outer) = (KEY_CORNER_PX, KEY_OUTER_CORNER_PX);
    let ((w, h), radii) = match target {
        KeyTarget::Grid(slot) => (
            (GRID_KEY_PX, GRID_KEY_PX),
            match slot.key {
                GridKey::TopLeft => [outer, k, k, k],
                GridKey::TopRight => [k, outer, k, k],
                _ => [k; 4],
            },
        ),
        KeyTarget::Control(button) if button.cid() == Some(Cid::LEFT_PAGING) => (
            PAGING_KEY_PX,
            [
                PAGING_TOP_CORNER_PX,
                PAGING_TOP_CORNER_PX,
                PAGING_INNER_CORNER_PX,
                PAGING_OUTER_CORNER_PX,
            ],
        ),
        KeyTarget::Control(button) if button.cid() == Some(Cid::RIGHT_PAGING) => (
            PAGING_KEY_PX,
            [
                PAGING_TOP_CORNER_PX,
                PAGING_TOP_CORNER_PX,
                PAGING_OUTER_CORNER_PX,
                PAGING_INNER_CORNER_PX,
            ],
        ),
        KeyTarget::Control(_) | KeyTarget::FunctionKey(_) => ((GRID_KEY_PX, GRID_KEY_PX), [k; 4]),
    };
    let scale = render_w / STOCK_KEYPAD_RENDER.0 as f32;
    KeyShape {
        w: w * scale,
        h: h * scale,
        radii: radii.map(|r| r * scale),
    }
}

/// The MX Creative Keypad's keys on its stock render, or `None` when
/// `asset` is not that render. Logi's depot marks none of the keypad's keys
/// — Options+ lays them out itself — so OpenLogi carries their centres,
/// measured off the render the tab shows, the way a marked depot would
/// place them.
#[expect(
    clippy::cast_precision_loss,
    reason = "render dimensions are a few thousand pixels at most"
)]
fn stock_keypad_slots(asset: &ResolvedAsset) -> Option<Vec<KeySlotLayout>> {
    if asset.depot != STOCK_KEYPAD_DEPOT
        || (asset.png_width, asset.png_height) != STOCK_KEYPAD_RENDER
    {
        return None;
    }
    let (w, h) = (STOCK_KEYPAD_RENDER.0 as f32, STOCK_KEYPAD_RENDER.1 as f32);
    Some(keypad_layout(
        |row, col| KeyPoint {
            x_frac: STOCK_GRID_COLS_PX[col] / w,
            y_frac: STOCK_GRID_ROWS_PX[row] / h,
        },
        |col| KeyPoint {
            x_frac: STOCK_PAGING_COLS_PX[col] / w,
            y_frac: STOCK_PAGING_ROW_PX / h,
        },
    ))
}

/// The depot's marked keys as key slots, each named by what its slot ID
/// decodes to: a control's `_c<decimal>` or a grid key's `_g<index>` (see
/// [`key_surface`]). The keys above the numpad and down the right edge of a
/// keyboard follow the F-row: one slot per key, ordered top row first, then
/// left to right.
///
/// One slot per key. The image groups are read in [`KEY_MARKER_IMAGES`]
/// order, so a key `device_keys_image` marks keeps that position even if
/// `device_buttons_image` marks it again elsewhere: the keys image is what
/// the marker calibration was measured against.
fn marker_slots(asset: &ResolvedAsset, keypad: Option<&KeypadDescriptor>) -> Vec<KeySlotLayout> {
    // Rows are banded by y; within a band, left to right. Markers on one
    // physical row differ by well under a band.
    const ROW_BAND: f32 = 0.06;
    // The canvas correction exists for Logitech-authored keyboard markers.
    // A keypad depot is OpenLogi's own — its markers are authored against
    // the render the photo shows, so their centres land as authored.
    let canvas_calibrated = keypad.is_none();
    let mut seen_controls = HashSet::new();
    let mut seen_grid = HashSet::new();
    let mut slots: Vec<KeySlotLayout> = KEY_MARKER_IMAGES
        .iter()
        .flat_map(|key| {
            asset
                .metadata
                .images
                .iter()
                .filter(move |img| img.key == *key)
        })
        .flat_map(|img| img.assignments.iter())
        .filter_map(|assignment| {
            let target = if let Some(cid) = assignment.control_id() {
                let button = ButtonId::control(cid);
                seen_controls
                    .insert(button)
                    .then_some(KeyTarget::Control(button))?
            } else {
                let key = GridKey::from_index(assignment.grid_index()?)?;
                seen_grid
                    .insert(key.index())
                    .then_some(KeyTarget::grid(key))?
            };
            let raw = KeyPoint {
                x_frac: assignment.marker.x / 100.0,
                y_frac: assignment.marker.y / 100.0,
            };
            Some(KeySlotLayout {
                legend: target.label(),
                point: if canvas_calibrated {
                    calibrated_marker_point(raw)
                } else {
                    raw
                },
                target,
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
    key_slots(asset, None)
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
