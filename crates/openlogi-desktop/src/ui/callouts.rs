//! Layout rules the mouse view and the Keys tab share for their callout
//! cards: spreading a row of cards along their controls without reordering
//! them, and telling whether two leaders cross.

/// Whether two polylines cross. Segments that only touch at an end do not
/// count, so two leaders meeting the same card edge are fine.
pub(crate) fn routes_cross(a: &[(f32, f32)], b: &[(f32, f32)]) -> bool {
    a.windows(2)
        .any(|s| b.windows(2).any(|t| segments_cross(s[0], s[1], t[0], t[1])))
}

fn segments_cross(a: (f32, f32), b: (f32, f32), c: (f32, f32), d: (f32, f32)) -> bool {
    fn side(p: (f32, f32), q: (f32, f32), r: (f32, f32)) -> i8 {
        let cross = (q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0);
        if cross > 1e-3 {
            1
        } else if cross < -1e-3 {
            -1
        } else {
            0
        }
    }
    side(a, b, c) * side(a, b, d) < 0 && side(c, d, a) * side(c, d, b) < 0
}

/// Positions for a row of cards that keeps the given order: each card as
/// close to its `ideal` position as possible (least squares), neighbours at
/// least `spacing` apart, every position within `lo..=hi`. `ideal` must
/// already read in the order the cards should, so a card never jumps past
/// its neighbour and the leaders keep their order. Cards that cannot all fit
/// overflow both ends evenly.
#[expect(
    clippy::cast_precision_loss,
    reason = "a row holds at most a few dozen callouts — well under f32 mantissa"
)]
pub(crate) fn spread_in_order(ideal: &[f32], spacing: f32, lo: f32, hi: f32) -> Vec<f32> {
    let offsets: Vec<f32> = (0..ideal.len())
        .map(|index| spacing * index as f32)
        .collect();
    spread_at_offsets(ideal, &offsets, lo, hi)
}

/// [`spread_in_order`] for neighbours that need different room: `offsets`
/// gives each card's least distance from the first one, non-decreasing, so
/// card `i + 1` stays at least `offsets[i + 1] - offsets[i]` past card `i`.
#[expect(
    clippy::cast_precision_loss,
    reason = "a row holds at most a few dozen callouts — well under f32 mantissa"
)]
pub(crate) fn spread_at_offsets(ideal: &[f32], offsets: &[f32], lo: f32, hi: f32) -> Vec<f32> {
    let Some(&last_offset) = offsets.last() else {
        return Vec::new();
    };
    // Shifting each target back by its offset turns "at least that far
    // apart" into "non-decreasing", which pooling adjacent violators solves
    // in one pass: a run that would cross is replaced by its mean.
    let mut runs: Vec<(f32, usize)> = Vec::with_capacity(ideal.len());
    for (target, offset) in ideal.iter().zip(offsets) {
        runs.push((target - offset, 1));
        while let [.., (sum_a, len_a), (sum_b, len_b)] = runs[..] {
            if sum_a / len_a as f32 <= sum_b / len_b as f32 {
                break;
            }
            runs.truncate(runs.len() - 2);
            runs.push((sum_a + sum_b, len_a + len_b));
        }
    }
    let last_start = hi - last_offset;
    let (first, last) = if last_start >= lo {
        (lo, last_start)
    } else {
        let centred = f32::midpoint(lo, last_start);
        (centred, centred)
    };
    runs.into_iter()
        .flat_map(|(sum, len)| std::iter::repeat_n((sum / len as f32).clamp(first, last), len))
        .zip(offsets)
        .map(|(start, offset)| start + offset)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_keeps_ideal_positions_that_already_fit() {
        assert_eq!(
            spread_in_order(&[10., 50., 120.], 30., 0., 200.),
            [10., 50., 120.]
        );
    }

    #[test]
    fn spread_pushes_a_crowded_run_apart_around_its_mean() {
        let ys = spread_in_order(&[100., 105., 110.], 20., 0., 300.);
        assert_eq!(ys, [85., 105., 125.]);
    }

    #[test]
    fn spread_never_reorders_and_stays_in_bounds() {
        let ys = spread_in_order(&[5., 0., 300.], 40., 20., 280.);
        assert_eq!(ys, [20., 60., 280.]);
    }

    #[test]
    fn spread_centres_a_row_too_long_for_its_bounds() {
        let ys = spread_in_order(&[0., 0., 0.], 100., 0., 100.);
        assert_eq!(ys, [-50., 50., 150.]);
    }

    #[test]
    fn crossing_and_touching_polylines_are_told_apart() {
        let down = [(0., 0.), (10., 10.)];
        let up = [(0., 10.), (10., 0.)];
        let beside = [(10., 10.), (20., 10.)];
        assert!(routes_cross(&down, &up));
        assert!(!routes_cross(&down, &beside));
    }
}
