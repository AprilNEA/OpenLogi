//! Where the Keys tab puts each key's callout card and how its leader runs.
//!
//! Every card sits where its key is: a top-row key's card directly above it,
//! a key down the board's right or left edge gets its card beside it. Band
//! cards alternate between two lanes so each can be about two key pitches
//! wide; a leader from the upper lane drops through the gap between two lower
//! cards. Cards that would overlap are pushed apart as little as possible
//! without changing their order (see [`spread_in_order`]), so no two leaders
//! cross.

use super::{CALLOUT_BAND_H, KEY_CALLOUT_H, KEY_CALLOUT_TOP_LOWER, KEY_CALLOUT_TOP_UPPER};
use crate::ui::callouts::spread_in_order;

/// Narrowest a band card gets on a crowded row.
const KEY_CALLOUT_MIN_W: f32 = 44.;
/// Widest a band card gets on a roomy row.
const KEY_CALLOUT_MAX_W: f32 = 112.;
/// Free space on each side of a leader that drops between two lower cards.
const LEADER_CLEARANCE: f32 = 7.;
/// How far below the lower lane a leader may start its 45° run toward a key
/// it does not sit straight above.
const RUN_GAP: f32 = 6.;
/// Width of a card beside the board.
const SIDE_CARD_W: f32 = 128.;
/// Space between the board render and a card beside it.
const SIDE_GAP: f32 = 24.;
/// Width a band of cards beside the board adds to the pane.
const SIDE_BAND_W: f32 = SIDE_GAP + SIDE_CARD_W;
/// Space between two cards stacked beside the board.
const SIDE_CARD_GAP: f32 = 8.;
/// Straight end a leader keeps before it meets a card beside the board.
const SIDE_STUB: f32 = 8.;
/// Markers within this fraction of the image height of the highest marker
/// form the top row.
const ROW_BAND: f32 = 0.06;
/// Below the top row, a key this close to an image edge (as a fraction of the
/// width) gets its card beside the board instead of above it.
const SIDE_COLUMN: f32 = 0.25;

/// One card's box in pane coordinates. Its height is [`KEY_CALLOUT_H`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CardRect {
    pub(super) left: f32,
    pub(super) top: f32,
    pub(super) width: f32,
}

/// The cards and leaders for one render size, by key index.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CalloutLayout {
    pub(super) cards: Vec<CardRect>,
    /// Each leader's corners from the card to the key, in pane coordinates.
    pub(super) leaders: Vec<Vec<(f32, f32)>>,
    /// Where the board render starts inside the pane.
    pub(super) image_left: f32,
    /// The pane's width: the render plus any band of cards beside it.
    pub(super) pane_w: f32,
}

/// Where a key's card goes: in the band above the board or in a column
/// beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Band {
    Top,
    Beside(Column),
}

/// Which edge of the board a column of cards sits beside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Column {
    Left,
    Right,
}

/// Lay out a card and a leader for each key marker (fractions of the
/// `img_w` × `img_h` render) in the callout band above the board and beside
/// it.
pub(super) fn callout_layout(markers: &[(f32, f32)], img_w: f32, img_h: f32) -> CalloutLayout {
    let bands = bands(markers);
    let image_left = if bands.contains(&Band::Beside(Column::Left)) {
        SIDE_BAND_W
    } else {
        0.
    };
    let pane_w = img_w + side_bands_width(markers);
    // Each key's marker in pane coordinates.
    let keys: Vec<(f32, f32)> = markers
        .iter()
        .map(|&(x, y)| (image_left + x * img_w, CALLOUT_BAND_H + y * img_h))
        .collect();
    let mut layout = CalloutLayout {
        cards: vec![
            CardRect {
                left: 0.,
                top: 0.,
                width: 0.,
            };
            markers.len()
        ],
        leaders: vec![Vec::new(); markers.len()],
        image_left,
        pane_w,
    };
    let in_band =
        |band: Band| -> Vec<usize> { (0..markers.len()).filter(|&i| bands[i] == band).collect() };
    place_band_cards(&mut layout, in_band(Band::Top), &keys);
    for column in [Column::Right, Column::Left] {
        let beside = in_band(Band::Beside(column));
        place_side_cards(&mut layout, beside, &keys, column, img_w, img_h);
    }
    layout
}

/// How much wider than the render the pane gets for the cards beside the
/// board: one band per side that has a key down its edge.
pub(super) fn side_bands_width(markers: &[(f32, f32)]) -> f32 {
    let bands = bands(markers);
    [Column::Left, Column::Right]
        .into_iter()
        .filter(|&column| bands.contains(&Band::Beside(column)))
        .map(|_| SIDE_BAND_W)
        .sum()
}

/// Which band each marker's card goes to: the top row and anything away from
/// the edges above the board, a key down an edge beside it.
fn bands(markers: &[(f32, f32)]) -> Vec<Band> {
    let top_row = markers
        .iter()
        .map(|&(_, y)| y)
        .fold(f32::INFINITY, f32::min);
    markers
        .iter()
        .map(|&(x, y)| {
            if y - top_row < ROW_BAND {
                Band::Top
            } else if x >= 1. - SIDE_COLUMN {
                Band::Beside(Column::Right)
            } else if x <= SIDE_COLUMN {
                Band::Beside(Column::Left)
            } else {
                Band::Top
            }
        })
        .collect()
}

/// The band above the board: each card centred over its key, the lanes
/// alternating left to right, every leader straight down when the card sits
/// over its key and with one 45° run below the lower lane when it was pushed
/// aside.
fn place_band_cards(layout: &mut CalloutLayout, mut top: Vec<usize>, keys: &[(f32, f32)]) {
    top.sort_by(|&a, &b| keys[a].0.total_cmp(&keys[b].0));
    let xs: Vec<f32> = top.iter().map(|&i| keys[i].0).collect();
    let width = band_card_width(&xs, layout.pane_w);
    let centres = spread_in_order(
        &xs,
        width / 2. + LEADER_CLEARANCE,
        width / 2.,
        layout.pane_w - width / 2.,
    );
    let run_y = KEY_CALLOUT_TOP_LOWER + KEY_CALLOUT_H + RUN_GAP;
    for (rank, (&i, cx)) in top.iter().zip(centres).enumerate() {
        let card_top = if rank % 2 == 0 {
            KEY_CALLOUT_TOP_LOWER
        } else {
            KEY_CALLOUT_TOP_UPPER
        };
        layout.cards[i] = CardRect {
            left: cx - width / 2.,
            top: card_top,
            width,
        };
        let (kx, ky) = keys[i];
        let start = (cx, card_top + KEY_CALLOUT_H);
        let offset = (kx - cx).abs();
        layout.leaders[i] = if offset < 0.5 {
            vec![start, (kx, ky)]
        } else {
            let run_end = (run_y + offset).min(ky);
            let mut leader = vec![start, (cx, run_y), (kx, run_end)];
            if run_end < ky {
                leader.push((kx, ky));
            }
            leader
        };
    }
}

/// A column of cards beside the board's `column` edge, each level with its
/// key where there is room.
fn place_side_cards(
    layout: &mut CalloutLayout,
    mut beside: Vec<usize>,
    keys: &[(f32, f32)],
    column: Column,
    img_w: f32,
    img_h: f32,
) {
    // The cards' board-facing edge, and the direction from the board out to
    // them.
    let (edge, outward) = match column {
        Column::Right => (layout.image_left + img_w + SIDE_GAP, 1.),
        Column::Left => (layout.image_left - SIDE_GAP, -1.),
    };
    beside.sort_by(|&a, &b| keys[a].1.total_cmp(&keys[b].1));
    let ys: Vec<f32> = beside.iter().map(|&i| keys[i].1).collect();
    let centres = spread_in_order(
        &ys,
        KEY_CALLOUT_H + SIDE_CARD_GAP,
        CALLOUT_BAND_H + KEY_CALLOUT_H / 2.,
        CALLOUT_BAND_H + img_h - KEY_CALLOUT_H / 2.,
    );
    for (&i, cy) in beside.iter().zip(centres) {
        let (kx, ky) = keys[i];
        layout.cards[i] = CardRect {
            left: match column {
                Column::Right => edge,
                Column::Left => edge - SIDE_CARD_W,
            },
            top: cy - KEY_CALLOUT_H / 2.,
            width: SIDE_CARD_W,
        };
        let rise = (cy - ky).abs();
        layout.leaders[i] = if rise < 0.5 {
            vec![(edge, cy), (kx, ky)]
        } else {
            let run_end = edge - outward * SIDE_STUB;
            let run_start = run_end - outward * rise;
            // A key too close to the card steepens the run rather than
            // starting it behind the key.
            let run_start = match column {
                Column::Right => run_start.max(kx),
                Column::Left => run_start.min(kx),
            };
            vec![(edge, cy), (run_end, cy), (run_start, ky), (kx, ky)]
        };
    }
}

/// Band cards span two key pitches less the room a leader needs between two
/// of them, within the width limits and never wider than the pane can hold.
#[expect(
    clippy::cast_precision_loss,
    reason = "a row holds at most a few dozen keys — well under f32 mantissa"
)]
fn band_card_width(xs: &[f32], pane_w: f32) -> f32 {
    let pitch = xs
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .fold(f32::INFINITY, f32::min);
    let gaps = xs.len().saturating_sub(1) as f32;
    // `gaps` neighbours `width / 2 + clearance` apart plus one full card.
    let fits = (pane_w - gaps * LEADER_CLEARANCE) / (gaps / 2. + 1.);
    (2. * pitch - 2. * LEADER_CLEARANCE)
        .min(fits)
        .clamp(KEY_CALLOUT_MIN_W, KEY_CALLOUT_MAX_W)
}
