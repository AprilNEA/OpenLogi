//! The pure geometry of a [`LogTrack`](super::LogTrack): where a value sits
//! on a logarithmic track, which ruler values it marks, which ruler labels
//! fit, and where value labels go so that none of them overlap.
//!
//! Positions are pixels from the track's leading edge; the component turns
//! them into layout and paint.

/// A logarithmic mapping between values from `min` to `max` and fractions of
/// the track's width: `min` at 0, `max` at 1, and every doubling the same
/// width apart.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LogScale {
    min: f32,
    ln_min: f32,
    ln_span: f32,
}

impl LogScale {
    /// The scale between `a` and `b`, in whichever order. A bound below 1
    /// is taken as 1, since a logarithmic track has no place for 0.
    pub(crate) fn new(a: f32, b: f32) -> Self {
        let (min, max) = (a.min(b).max(1.), a.max(b).max(1.));
        Self {
            min,
            ln_min: min.ln(),
            ln_span: max.ln() - min.ln(),
        }
    }

    /// Where `value` sits, from 0 at the minimum to 1 at the maximum; a
    /// value outside the range sits at the nearer end.
    pub(crate) fn fraction(self, value: f32) -> f32 {
        if self.ln_span <= 0. {
            return 0.;
        }
        ((value.max(self.min).ln() - self.ln_min) / self.ln_span).clamp(0., 1.)
    }

    /// The value at `fraction` of the track, clamped to the range.
    pub(crate) fn value_at(self, fraction: f32) -> f32 {
        (self.ln_min + self.ln_span * fraction.clamp(0., 1.)).exp()
    }

    /// The value under `x` pixels from the leading edge of a track `width`
    /// pixels wide.
    pub(crate) fn value_at_x(self, x: f32, width: f32) -> f32 {
        if width <= 0. {
            return self.min;
        }
        self.value_at(x / width)
    }
}

/// The ruler values of a doubling scale: `min`, then each doubling of it
/// below `max`, then `max` itself — 200, 400, …, 6400, 8200 for a range of
/// 200 to 8200.
pub(crate) fn doubling_ticks(min: u16, max: u16) -> Vec<u16> {
    let mut ticks = Vec::new();
    let mut tick = u32::from(min.max(1));
    while tick < u32::from(max) {
        // `tick` is below `max`, so it fits.
        ticks.push(u16::try_from(tick).unwrap_or(max));
        tick *= 2;
    }
    ticks.push(max);
    ticks
}

/// One text label on the track: the pixel its centre is anchored to, and
/// its width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Label {
    pub(crate) anchor: f32,
    pub(crate) width: f32,
}

impl Label {
    /// The left edge of the label centred on its anchor, pulled inside a
    /// track `track` pixels wide so a label at either end is not cut off.
    pub(crate) fn left(self, track: f32) -> f32 {
        (self.anchor - self.width / 2.).clamp(0., (track - self.width).max(0.))
    }
}

/// Which of the ruler `labels`, in track order, to draw on a track `track`
/// pixels wide: both ends, and each label between them that keeps `gap`
/// pixels from the labels drawn beside it, from the leading end on. A ruler
/// too narrow for both ends keeps the leading one.
pub(crate) fn ruler_labels(labels: &[Label], track: f32, gap: f32) -> Vec<bool> {
    let mut shown = vec![false; labels.len()];
    let (Some(first), Some(last)) = (labels.first(), labels.last()) else {
        return shown;
    };
    shown[0] = true;
    let mut drawn_right = first.left(track) + first.width;
    let last_left = last.left(track);
    let last_fits = labels.len() > 1 && last_left >= drawn_right + gap;
    let end = if last_fits {
        shown[labels.len() - 1] = true;
        last_left
    } else {
        track
    };
    for (index, label) in labels
        .iter()
        .enumerate()
        .take(labels.len().saturating_sub(1))
        .skip(1)
    {
        let left = label.left(track);
        let right = left + label.width;
        let clear_of_end = !last_fits || right + gap <= end;
        if left >= drawn_right + gap && clear_of_end {
            shown[index] = true;
            drawn_right = right;
        }
    }
    shown
}

/// The left edges of `labels`, in track order, moved apart just enough to
/// keep `gap` pixels between neighbours and kept inside a track `track`
/// pixels wide. Labels that cannot all fit overlap only at the leading end.
pub(crate) fn spread_labels(labels: &[Label], track: f32, gap: f32) -> Vec<f32> {
    let mut lefts: Vec<f32> = labels.iter().map(|label| label.left(track)).collect();
    for index in 1..lefts.len() {
        let floor = lefts[index - 1] + labels[index - 1].width + gap;
        lefts[index] = lefts[index].max(floor);
    }
    for index in (0..lefts.len()).rev() {
        let ceiling = match lefts.get(index + 1) {
            Some(next) => next - gap - labels[index].width,
            None => track - labels[index].width,
        };
        lefts[index] = lefts[index].min(ceiling).max(0.);
    }
    lefts
}

/// Where each dot anchored at `anchors`, in track order, answers the
/// pointer, as left and right pixels: `reach` either side of it, but only
/// up to halfway to the dot beside it, so that dots closer than `reach`
/// split the space between them and none hides another.
pub(crate) fn hit_spans(anchors: &[f32], reach: f32) -> Vec<(f32, f32)> {
    anchors
        .iter()
        .enumerate()
        .map(|(index, &anchor)| {
            let mut left = anchor - reach;
            let mut right = anchor + reach;
            if let Some(lower) = index.checked_sub(1).map(|lower| anchors[lower]) {
                left = left.max(f32::midpoint(lower, anchor));
            }
            if let Some(&upper) = anchors.get(index + 1) {
                right = right.min(f32::midpoint(anchor, upper));
            }
            (left, right.max(left))
        })
        .collect()
}

/// Which of the `spans`, as left and right pixels, lies under `x`: the
/// first one that does.
pub(crate) fn label_at(x: f32, spans: &[(f32, f32)]) -> Option<usize> {
    spans
        .iter()
        .position(|(left, right)| (*left..=*right).contains(&x))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn each_doubling_takes_the_same_width() {
        let scale = LogScale::new(200., 8200.);
        let step = scale.fraction(400.) - scale.fraction(200.);

        assert!(close(scale.fraction(200.), 0.));
        assert!(close(scale.fraction(8200.), 1.));
        for low in [400., 800., 1600., 3200.] {
            let width = scale.fraction(low * 2.) - scale.fraction(low);
            assert!(close(width, step), "{low}..{}", low * 2.);
        }
        // 1600 is three doublings of 200, out of log2(41) for the range.
        assert!(close(scale.fraction(1600.), 3. / 41_f32.log2()));
    }

    #[test]
    fn a_position_maps_back_to_its_value_and_ends_clamp() {
        let scale = LogScale::new(8200., 200.);

        for value in [200., 333., 1600., 5000., 8200.] {
            assert!(
                (scale.value_at(scale.fraction(value)) - value).abs() < 0.05,
                "{value}"
            );
        }
        assert!(close(scale.fraction(50.), 0.));
        assert!(close(scale.fraction(20_000.), 1.));
        assert!(close(scale.value_at(-0.5), 200.));
        assert!((scale.value_at(1.5) - 8200.).abs() < 0.05);
        assert!((scale.value_at_x(250., 500.) - scale.value_at(0.5)).abs() < 0.05);
        assert!(close(scale.value_at_x(10., 0.), 200.));
    }

    #[test]
    fn a_range_without_width_puts_everything_at_the_start() {
        let scale = LogScale::new(400., 400.);

        assert!(close(scale.fraction(400.), 0.));
        assert!(close(scale.fraction(9000.), 0.));
        assert!(close(scale.value_at(0.7), 400.));
    }

    #[test]
    fn the_ruler_doubles_from_the_minimum_and_ends_at_the_maximum() {
        assert_eq!(
            doubling_ticks(200, 8200),
            [200, 400, 800, 1600, 3200, 6400, 8200]
        );
        // A maximum that is itself a doubling is not repeated.
        assert_eq!(doubling_ticks(100, 800), [100, 200, 400, 800]);
        assert_eq!(doubling_ticks(500, 500), [500]);
        assert_eq!(doubling_ticks(0, 3), [1, 2, 3]);
        assert_eq!(
            doubling_ticks(40_000, u16::MAX),
            [40_000, u16::MAX],
            "a doubling past u16 stops the ruler"
        );
    }

    fn labels(anchors: &[f32], width: f32) -> Vec<Label> {
        anchors
            .iter()
            .map(|&anchor| Label { anchor, width })
            .collect()
    }

    #[test]
    fn a_wide_ruler_draws_every_label() {
        let ruler = labels(&[0., 100., 200., 300., 400.], 30.);

        assert_eq!(ruler_labels(&ruler, 400., 6.), [true; 5]);
    }

    #[test]
    fn a_narrow_ruler_keeps_both_ends_and_drops_what_would_overlap() {
        // The doubling ruler of 200..8200 on a 400 px track: 6400 sits 27 px
        // from 8200, too close for two 28 px labels.
        let scale = LogScale::new(200., 8200.);
        let ruler: Vec<Label> = doubling_ticks(200, 8200)
            .into_iter()
            .map(|tick| Label {
                anchor: scale.fraction(f32::from(tick)) * 400.,
                width: 28.,
            })
            .collect();

        let shown = ruler_labels(&ruler, 400., 6.);

        assert_eq!(shown, [true, true, true, true, true, false, true]);
        let drawn: Vec<(f32, f32)> = ruler
            .iter()
            .zip(&shown)
            .filter(|(_, shown)| **shown)
            .map(|(label, _)| (label.left(400.), label.left(400.) + label.width))
            .collect();
        for pair in drawn.windows(2) {
            assert!(pair[1].0 >= pair[0].1 + 6., "{pair:?} overlap");
        }
    }

    #[test]
    fn crowded_labels_between_the_ends_are_thinned_from_the_leading_end() {
        let ruler = labels(&[0., 20., 40., 60., 80., 100.], 24.);

        assert_eq!(
            ruler_labels(&ruler, 100., 4.),
            [true, false, true, false, false, true]
        );
    }

    #[test]
    fn a_ruler_too_narrow_for_both_ends_keeps_the_leading_one() {
        let ruler = labels(&[0., 30.], 28.);

        assert_eq!(ruler_labels(&ruler, 30., 6.), [true, false]);
        assert!(ruler_labels(&[], 30., 6.).is_empty());
        assert_eq!(ruler_labels(&labels(&[5.], 28.), 30., 6.), [true]);
    }

    #[test]
    fn end_labels_stay_inside_the_track() {
        assert!(close(
            Label {
                anchor: 0.,
                width: 30.
            }
            .left(300.),
            0.
        ));
        assert!(close(
            Label {
                anchor: 300.,
                width: 30.
            }
            .left(300.),
            270.
        ));
        assert!(close(
            Label {
                anchor: 150.,
                width: 30.
            }
            .left(300.),
            135.
        ));
    }

    #[test]
    fn value_labels_that_would_touch_are_pushed_apart() {
        // 800 and 850 sit a few pixels apart; their labels move apart
        // around them, and a lone label stays centred on its dot.
        let spread = spread_labels(&labels(&[100., 104., 300.], 30.), 400., 4.);

        assert_eq!(spread.len(), 3);
        assert!(close(spread[0], 85.));
        assert!(close(spread[1], 119.));
        assert!(close(spread[2], 285.));
    }

    #[test]
    fn value_labels_crowded_at_the_far_end_move_back_inside() {
        let spread = spread_labels(&labels(&[396., 398., 400.], 30.), 400., 4.);

        assert!(close(spread[2], 370.));
        assert!(close(spread[1], 336.));
        assert!(close(spread[0], 302.));
    }

    #[test]
    fn value_labels_that_cannot_fit_overlap_only_at_the_leading_end() {
        let spread = spread_labels(&labels(&[0., 1., 2.], 30.), 60., 4.);

        assert!(close(spread[2], 30.));
        assert!(close(spread[1], 0.));
        assert!(close(spread[0], 0.));
    }

    #[test]
    fn a_lone_dot_answers_its_whole_reach() {
        assert_eq!(hit_spans(&[100.], 12.), [(88., 112.)]);
        assert!(hit_spans(&[], 12.).is_empty());
    }

    #[test]
    fn dots_closer_than_their_reach_split_the_space_between_them() {
        // 8150 and 8200 sit about 1.4 px apart on a 500 px track.
        let spans = hit_spans(&[498.6, 500., 560.], 12.);

        assert!(close(spans[0].0, 486.6));
        assert!(close(spans[0].1, 499.3));
        assert!(close(spans[1].0, 499.3));
        assert!(close(spans[1].1, 512.));
        assert!(
            spans[0].1 - spans[0].0 > 12.,
            "the lower dot keeps its own side"
        );
        // Far apart, each keeps its whole reach.
        assert!(close(spans[2].0, 548.) && close(spans[2].1, 572.));
    }

    #[test]
    fn dots_at_the_same_pixel_each_keep_one_side() {
        let spans = hit_spans(&[200., 200., 200.], 10.);

        assert_eq!(spans[0], (190., 200.));
        assert_eq!(
            spans[1],
            (200., 200.),
            "the middle one is caught by neither side"
        );
        assert_eq!(spans[2], (200., 210.));
    }

    #[test]
    fn a_label_is_found_under_the_pointer() {
        let spans = [(10., 40.), (50., 80.)];

        assert_eq!(label_at(10., &spans), Some(0));
        assert_eq!(label_at(60., &spans), Some(1));
        assert_eq!(label_at(45., &spans), None);
        assert_eq!(label_at(90., &spans), None);
    }
}
