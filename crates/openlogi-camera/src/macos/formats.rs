//! Which stream modes a camera's formats offer, and which format a mode needs.
//!
//! `AVCaptureDevice.formats` lists one entry per frame size *and* pixel format
//! (a camera that streams both YUYV and MJPEG at 1080p lists 1080p twice), each
//! with the frame-rate ranges it runs at. This module turns that list into the
//! platform-independent [`StreamModes`], and back: for a chosen mode, which
//! format and range to activate. Plain data in and out, so the decisions are
//! testable without a camera; reading the list is `macos.rs`, activating the
//! pick is `capture_macos.rs`.

use crate::{StreamMode, StreamModes};

/// The rates offered inside a range that spans several, fastest first.
///
/// A UVC camera reports each rate as a range of one, so these only name the
/// rates of a range that genuinely varies (a built-in camera's 1–30 fps).
/// Only rates that fit inside the range are offered, because asking a device
/// for a duration outside its range raises an exception rather than failing.
const SPANNED_RATES: [u32; 11] = [120, 90, 60, 50, 30, 25, 24, 20, 15, 10, 5];

/// One `AVFrameRateRange`: the slowest and fastest rate a format runs at, in
/// fps as AVFoundation reports them (a UVC camera's 30 fps is `30.00003`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RateRange {
    pub min: f64,
    pub max: f64,
}

impl RateRange {
    /// Whether the range names one rate. UVC cameras report each of theirs this
    /// way, so a range that rounds to a single whole rate counts too.
    fn is_single(self) -> bool {
        round_fps(self.min) == round_fps(self.max)
    }

    /// The whole rates this range offers.
    fn rates(self) -> Vec<u32> {
        if self.is_single() {
            return vec![round_fps(self.max)];
        }
        SPANNED_RATES
            .into_iter()
            .filter(|&rate| self.min <= f64::from(rate) && f64::from(rate) <= self.max)
            .collect()
    }
}

/// One `AVCaptureDeviceFormat`, as the device lists it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DeviceFormat {
    pub width: u32,
    pub height: u32,
    /// The format's `videoSupportedFrameRateRanges`, in the device's order.
    pub rates: Vec<RateRange>,
}

/// How to set the frame duration once a format is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameTiming {
    /// The range names exactly one rate: use the durations it reports. They are
    /// not `1/fps`: the `30` a UVC camera lists is `30.00003` fps, and a request
    /// for exactly 1/30 s falls outside the range it was read from.
    Reported,
    /// The range spans rates: ask for exactly `1/fps` seconds.
    Rate(u32),
}

/// A mode, and where in the device's lists to find what activating it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Selection {
    /// The mode the camera will stream at.
    pub mode: StreamMode,
    /// Index into the device's `formats`.
    pub format: usize,
    /// Index into that format's `videoSupportedFrameRateRanges`.
    pub range: usize,
    /// How to set the frame duration.
    pub timing: FrameTiming,
}

/// Round a frame rate to the nearest whole fps (so 59.94 reads as 60). `0` for
/// a rate that is not a positive finite number.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "fps is rounded, finite, and clamped to a small non-negative range"
)]
pub(crate) fn round_fps(rate: f64) -> u32 {
    if rate.is_finite() && rate > 0.0 {
        rate.round() as u32
    } else {
        0
    }
}

/// Every mode `formats` offer. Formats of one size and different pixel formats
/// merge into one list of rates; empty sizes and rates are dropped by
/// [`StreamModes::new`].
pub(crate) fn offered(formats: &[DeviceFormat]) -> StreamModes {
    StreamModes::new(formats.iter().flat_map(|format| {
        format.rates.iter().flat_map(move |range| {
            range.rates().into_iter().map(move |fps| StreamMode {
                width: format.width,
                height: format.height,
                fps,
            })
        })
    }))
}

/// What to activate to stream at `requested`: that mode when the camera offers
/// it, else the camera's default ([`StreamModes::resolve`]). `None` when the
/// camera offers no mode at all.
///
/// When several formats offer the mode, the first the device lists wins, so the
/// pick is the same on every open.
pub(crate) fn choose(formats: &[DeviceFormat], requested: Option<StreamMode>) -> Option<Selection> {
    let mode = offered(formats).resolve(requested)?;
    formats
        .iter()
        .enumerate()
        .filter(|(_, format)| (format.width, format.height) == mode.resolution())
        .find_map(|(format, listed)| {
            listed
                .rates
                .iter()
                .enumerate()
                .find(|(_, range)| range.rates().contains(&mode.fps))
                .map(|(range, found)| Selection {
                    mode,
                    format,
                    range,
                    timing: if found.is_single() {
                        FrameTiming::Reported
                    } else {
                        FrameTiming::Rate(mode.fps)
                    },
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn mode(width: u32, height: u32, fps: u32) -> StreamMode {
        StreamMode { width, height, fps }
    }

    /// A format whose ranges are `(min, max)` fps pairs.
    fn format(width: u32, height: u32, ranges: &[(f64, f64)]) -> DeviceFormat {
        DeviceFormat {
            width,
            height,
            rates: ranges
                .iter()
                .map(|&(min, max)| RateRange { min, max })
                .collect(),
        }
    }

    /// A UVC camera's rate: a range of one, a hair above the whole number.
    fn uvc(fps: f64) -> (f64, f64) {
        (fps + 0.00003, fps + 0.00003)
    }

    #[test]
    fn a_uvc_range_of_one_reads_as_the_whole_rate() {
        let formats = [format(1920, 1080, &[uvc(60.0), uvc(30.0)])];
        assert_eq!(
            offered(&formats).iter().collect::<Vec<_>>(),
            [mode(1920, 1080, 60), mode(1920, 1080, 30)]
        );
    }

    #[test]
    fn ntsc_rates_round_to_the_whole_rate() {
        let formats = [format(1280, 720, &[(29.97, 29.97), (59.94, 59.94)])];
        assert_eq!(offered(&formats).rates_at((1280, 720)), [60, 30]);
    }

    #[test]
    fn one_size_in_two_pixel_formats_merges_into_one_list_of_rates() {
        // YUYV over USB 2 manages 5 fps at 1080p; MJPEG does 30 and 60.
        let formats = [
            format(1920, 1080, &[uvc(5.0)]),
            format(1920, 1080, &[uvc(30.0), uvc(60.0)]),
        ];
        let modes = offered(&formats);
        assert_eq!(modes.resolutions(), [(1920, 1080)]);
        assert_eq!(modes.rates_at((1920, 1080)), [60, 30, 5]);
    }

    #[test]
    fn a_range_that_spans_rates_offers_the_standard_ones_inside_it() {
        let formats = [format(1280, 720, &[(1.0, 30.0)])];
        assert_eq!(
            offered(&formats).rates_at((1280, 720)),
            [30, 25, 24, 20, 15, 10, 5]
        );
    }

    #[test]
    fn a_spanning_range_never_offers_a_rate_outside_it() {
        // 30 fps would be a 1/30 s duration, shorter than this camera's floor.
        let formats = [format(1280, 720, &[(10.0, 29.97)])];
        let rates = offered(&formats).rates_at((1280, 720));
        assert_eq!(rates, [25, 24, 20, 15, 10]);
    }

    #[test]
    fn frames_and_rates_the_device_reports_empty_are_not_offered() {
        let formats = [
            format(0, 0, &[uvc(30.0)]),
            format(1280, 720, &[(0.0, 0.0)]),
            format(640, 480, &[]),
        ];
        assert!(offered(&formats).is_empty());
        assert_eq!(choose(&formats, None), None);
    }

    #[test]
    fn choosing_finds_the_format_that_offers_the_rate() {
        let formats = [
            format(1920, 1080, &[uvc(5.0)]),
            format(1280, 720, &[uvc(30.0)]),
            format(1920, 1080, &[uvc(30.0), uvc(60.0)]),
        ];
        let selection = choose(&formats, Some(mode(1920, 1080, 60))).unwrap();
        assert_eq!(selection.mode, mode(1920, 1080, 60));
        assert_eq!((selection.format, selection.range), (2, 1));
        assert_eq!(selection.timing, FrameTiming::Reported);
    }

    #[test]
    fn the_first_format_offering_a_mode_wins() {
        let formats = [
            format(1280, 720, &[uvc(30.0)]),
            format(1280, 720, &[uvc(30.0)]),
        ];
        let selection = choose(&formats, Some(mode(1280, 720, 30))).unwrap();
        assert_eq!((selection.format, selection.range), (0, 0));
    }

    #[test]
    fn a_spanning_range_is_asked_for_exactly_the_rate_chosen() {
        let formats = [format(1280, 720, &[(1.0, 30.0)])];
        let selection = choose(&formats, Some(mode(1280, 720, 24))).unwrap();
        assert_eq!(selection.timing, FrameTiming::Rate(24));
    }

    #[test]
    fn a_request_the_camera_does_not_offer_opens_at_its_default() {
        let formats = [
            format(1920, 1080, &[uvc(30.0), uvc(60.0)]),
            format(1280, 720, &[uvc(30.0), uvc(60.0)]),
        ];
        // 24 fps is offered at neither size.
        let selection = choose(&formats, Some(mode(1920, 1080, 24))).unwrap();
        assert_eq!(selection.mode, mode(1280, 720, 30));
        assert_eq!(choose(&formats, None).unwrap().mode, mode(1280, 720, 30));
    }

    #[test]
    fn round_fps_rejects_what_is_not_a_rate() {
        assert_eq!(round_fps(59.94), 60);
        assert_eq!(round_fps(0.0), 0);
        assert_eq!(round_fps(-5.0), 0);
        assert_eq!(round_fps(f64::NAN), 0);
        assert_eq!(round_fps(f64::INFINITY), 0);
    }
}
