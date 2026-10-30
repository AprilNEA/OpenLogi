//! The frame sizes and rates a camera can stream, and how a request for one is
//! resolved against what the camera actually offers.
//!
//! Unlike brightness or zoom, resolution and frame rate are not registers the
//! camera holds: UVC negotiates them with the camera every time a stream opens
//! (the probe/commit handshake behind V4L2's set-format and set-parameters
//! ioctls). A mode is therefore the preview's own: Meet, Zoom or OBS each
//! negotiate theirs when they open the camera, so a choice here shapes
//! OpenLogi's preview and nothing else.
//!
//! Everything here is platform-independent. A backend enumerates the modes its
//! camera offers ([`StreamModes`]) and opens the stream at the mode it is
//! handed; the decisions in between — what a missing request means, which rate
//! survives a change of resolution — are made once, here.

use std::cmp::Reverse;
use std::fmt;

/// The size a stream opens at when nothing else is asked for: sharp in the
/// preview box at a fraction of a 4K frame's decode, copy and upload cost.
pub(crate) const DEFAULT_RESOLUTION: (u32, u32) = (1280, 720);

/// The rate a stream opens at when nothing else is asked for.
pub(crate) const DEFAULT_FPS: u32 = 30;

/// One frame size and rate a camera can stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamMode {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Whole frames per second. Drivers report a frame *interval*; it is
    /// rounded to the nearest whole rate, so an NTSC `29.97` reads as `30`.
    pub fps: u32,
}

impl StreamMode {
    /// The frame size as `(width, height)`.
    #[must_use]
    pub const fn resolution(self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl fmt::Display for StreamMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{} @ {} fps", self.width, self.height, self.fps)
    }
}

/// Whole frames per second for a rate of `numerator / denominator` fps: the
/// nearest whole number, not a truncation, because the rates cameras report are
/// rounded the way their firmware rounds them — a UVC camera's `30` is often
/// `10000000 / 333333`, and `7.5` reads as `8`. `None` for a zero denominator,
/// or a rate too slow to round to one frame per second (a driver reporting a
/// zero interval would otherwise divide by zero).
///
/// The one place that decides this for every backend that reports a ratio:
/// Linux reads the reciprocal (V4L2 intervals are seconds per frame), Windows
/// reads the rate as is.
#[cfg(any(test, target_os = "linux", target_os = "windows"))]
pub(crate) fn whole_fps(numerator: u32, denominator: u32) -> Option<u32> {
    if denominator == 0 {
        return None;
    }
    let (numerator, denominator) = (u64::from(numerator), u64::from(denominator));
    // Rounds half up, in integers: no float to truncate, and in `u64` the
    // rounding add cannot overflow whatever a hostile driver reports.
    u32::try_from((2 * numerator + denominator) / (2 * denominator))
        .ok()
        .filter(|fps| *fps > 0)
}

/// Pixel count of a frame size, for ordering and for finding the nearest one.
const fn area((width, height): (u32, u32)) -> u64 {
    // `as` is a lossless widening here; `From` is not usable in a `const fn`.
    width as u64 * height as u64
}

/// Every [`StreamMode`] a camera offers for the pixel format its backend
/// streams, largest frame first and fastest rate first within a frame size.
///
/// The ordering is part of the contract: a UI lists resolutions and rates in
/// the order they come out of here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamModes(Vec<StreamMode>);

impl StreamModes {
    /// Collect `modes`, dropping empty sizes and rates (a driver reporting a
    /// zero interval would otherwise divide by zero further on), duplicates,
    /// and ordering what is left.
    #[must_use]
    pub fn new(modes: impl IntoIterator<Item = StreamMode>) -> Self {
        let mut modes: Vec<StreamMode> = modes
            .into_iter()
            .filter(|mode| mode.width > 0 && mode.height > 0 && mode.fps > 0)
            .collect();
        // Width settles a tie between frame sizes of equal area, and with the
        // area it fixes the height, so equal modes end up adjacent.
        modes.sort_by_key(|mode| {
            (
                Reverse(area(mode.resolution())),
                Reverse(mode.width),
                Reverse(mode.fps),
            )
        });
        modes.dedup();
        Self(modes)
    }

    /// Whether the camera offers no selectable mode at all — because its
    /// backend cannot list them, or its driver reports only stepwise ranges.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether the camera offers exactly `mode`.
    #[must_use]
    pub fn contains(&self, mode: StreamMode) -> bool {
        self.0.contains(&mode)
    }

    /// Every mode, in the order described on [`StreamModes`].
    pub fn iter(&self) -> impl Iterator<Item = StreamMode> + '_ {
        self.0.iter().copied()
    }

    /// The distinct frame sizes on offer, largest first.
    #[must_use]
    pub fn resolutions(&self) -> Vec<(u32, u32)> {
        let mut sizes: Vec<(u32, u32)> = Vec::new();
        for mode in self.iter() {
            if !sizes.contains(&mode.resolution()) {
                sizes.push(mode.resolution());
            }
        }
        sizes
    }

    /// The frame rates on offer at `resolution`, fastest first. Empty when the
    /// camera does not offer that size.
    #[must_use]
    pub fn rates_at(&self, resolution: (u32, u32)) -> Vec<u32> {
        self.iter()
            .filter(|mode| mode.resolution() == resolution)
            .map(|mode| mode.fps)
            .collect()
    }

    /// The mode a stream opens at when nothing was asked for: the frame size
    /// nearest 1280×720 by pixel count (the larger one on a tie), at the rate
    /// nearest 30 fps (the slower one on a tie). `None` for an empty list.
    #[must_use]
    pub fn default_mode(&self) -> Option<StreamMode> {
        let wanted = area(DEFAULT_RESOLUTION);
        let resolution = self
            .resolutions()
            .into_iter()
            .min_by_key(|&size| (area(size).abs_diff(wanted), Reverse(area(size))))?;
        self.nearest_rate(resolution, DEFAULT_FPS)
    }

    /// The mode a stream should actually open at for `requested`: that very
    /// mode when the camera offers it, else [`Self::default_mode`].
    ///
    /// A saved choice can outlive what the camera offers — another unit of the
    /// same model, a firmware change — so a request is never trusted blindly.
    /// Backends open the stream at this, and a UI shows this as the selection,
    /// so the two cannot disagree. `None` only for an empty list.
    #[must_use]
    pub fn resolve(&self, requested: Option<StreamMode>) -> Option<StreamMode> {
        requested
            .filter(|mode| self.contains(*mode))
            .or_else(|| self.default_mode())
    }

    /// The mode to move to when the user picks `resolution` while streaming at
    /// `current`: the same rate if the new size offers it, else the nearest
    /// one it does (the slower on a tie). `None` when the camera does not offer
    /// that size.
    #[must_use]
    pub fn with_resolution(
        &self,
        current: StreamMode,
        resolution: (u32, u32),
    ) -> Option<StreamMode> {
        self.nearest_rate(resolution, current.fps)
    }

    /// The mode at `resolution` whose rate is closest to `fps`, the slower one
    /// on a tie.
    fn nearest_rate(&self, resolution: (u32, u32), fps: u32) -> Option<StreamMode> {
        self.iter()
            .filter(|mode| mode.resolution() == resolution)
            .min_by_key(|mode| (mode.fps.abs_diff(fps), mode.fps))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn mode(width: u32, height: u32, fps: u32) -> StreamMode {
        StreamMode { width, height, fps }
    }

    /// The shape of a StreamCam's MJPEG list, abridged: two sizes with 60, and
    /// a small one without.
    fn streamcam() -> StreamModes {
        StreamModes::new([
            mode(640, 360, 30),
            mode(1280, 720, 30),
            mode(1920, 1080, 30),
            mode(1920, 1080, 60),
            mode(1280, 720, 60),
            mode(1280, 720, 24),
        ])
    }

    #[test]
    fn modes_list_the_largest_frame_first_and_the_fastest_rate_first() {
        let listed: Vec<_> = streamcam().iter().collect();
        assert_eq!(
            listed,
            [
                mode(1920, 1080, 60),
                mode(1920, 1080, 30),
                mode(1280, 720, 60),
                mode(1280, 720, 30),
                mode(1280, 720, 24),
                mode(640, 360, 30),
            ]
        );
    }

    #[test]
    fn duplicates_and_empty_modes_are_dropped() {
        let modes = StreamModes::new([
            mode(1280, 720, 30),
            mode(1280, 720, 30),
            mode(0, 720, 30),
            mode(1280, 0, 30),
            // A zero rate is what a zero interval from the driver becomes.
            mode(1280, 720, 0),
        ]);
        assert_eq!(modes.iter().collect::<Vec<_>>(), [mode(1280, 720, 30)]);
    }

    #[test]
    fn frames_of_equal_area_stay_distinct_and_ordered() {
        let modes = StreamModes::new([mode(960, 960, 30), mode(1280, 720, 30)]);
        assert_eq!(modes.resolutions(), [(1280, 720), (960, 960)]);
    }

    #[test]
    fn resolutions_are_distinct_and_rates_follow_their_frame() {
        let modes = streamcam();
        assert_eq!(modes.resolutions(), [(1920, 1080), (1280, 720), (640, 360)]);
        assert_eq!(modes.rates_at((1280, 720)), [60, 30, 24]);
        assert_eq!(modes.rates_at((640, 360)), [30]);
        assert!(modes.rates_at((800, 600)).is_empty());
    }

    #[test]
    fn the_default_is_720p_at_30_when_offered() {
        assert_eq!(streamcam().default_mode(), Some(mode(1280, 720, 30)));
    }

    #[test]
    fn the_default_rate_is_the_nearest_offered_and_the_slower_on_a_tie() {
        let nearest = StreamModes::new([mode(1280, 720, 60), mode(1280, 720, 24)]);
        assert_eq!(nearest.default_mode(), Some(mode(1280, 720, 24)));

        // 20 and 40 are both 10 away from 30: take the cheaper stream.
        let tie = StreamModes::new([mode(1280, 720, 40), mode(1280, 720, 20)]);
        assert_eq!(tie.default_mode(), Some(mode(1280, 720, 20)));
    }

    #[test]
    fn the_default_frame_is_the_nearest_to_720p_when_it_is_not_offered() {
        // 1024x768 is closer in area to 720p than 640x480 or 1920x1080 are.
        let modes = StreamModes::new([
            mode(640, 480, 30),
            mode(1024, 768, 30),
            mode(1920, 1080, 30),
        ]);
        assert_eq!(modes.default_mode(), Some(mode(1024, 768, 30)));

        // A camera with only one size has only one answer.
        let only = StreamModes::new([mode(3840, 2160, 30)]);
        assert_eq!(only.default_mode(), Some(mode(3840, 2160, 30)));
    }

    #[test]
    fn an_empty_list_has_no_default_and_resolves_nothing() {
        let empty = StreamModes::default();
        assert!(empty.is_empty());
        assert_eq!(empty.default_mode(), None);
        assert_eq!(empty.resolve(Some(mode(1280, 720, 30))), None);
        assert_eq!(empty.resolve(None), None);
    }

    #[test]
    fn an_offered_request_is_honoured() {
        assert_eq!(
            streamcam().resolve(Some(mode(1920, 1080, 60))),
            Some(mode(1920, 1080, 60))
        );
    }

    #[test]
    fn a_request_the_camera_does_not_offer_falls_back_to_the_default() {
        let modes = streamcam();
        // The size is offered, this rate at it is not.
        assert_eq!(
            modes.resolve(Some(mode(1920, 1080, 24))),
            modes.default_mode()
        );
        // Not offered at all — a saved choice from another unit.
        assert_eq!(
            modes.resolve(Some(mode(4096, 2160, 30))),
            modes.default_mode()
        );
        assert_eq!(modes.resolve(None), modes.default_mode());
    }

    #[test]
    fn a_new_resolution_keeps_the_rate_when_it_is_offered() {
        let modes = streamcam();
        assert_eq!(
            modes.with_resolution(mode(1280, 720, 60), (1920, 1080)),
            Some(mode(1920, 1080, 60))
        );
    }

    #[test]
    fn a_new_resolution_takes_the_nearest_rate_when_it_is_not() {
        let modes = streamcam();
        // 640x360 only does 30.
        assert_eq!(
            modes.with_resolution(mode(1920, 1080, 60), (640, 360)),
            Some(mode(640, 360, 30))
        );
        // 24 is not offered at 1080p; 30 is nearer than 60.
        assert_eq!(
            modes.with_resolution(mode(1280, 720, 24), (1920, 1080)),
            Some(mode(1920, 1080, 30))
        );
    }

    #[test]
    fn a_resolution_the_camera_does_not_offer_is_not_a_choice() {
        assert_eq!(
            streamcam().with_resolution(mode(1280, 720, 30), (800, 600)),
            None
        );
    }

    #[test]
    fn a_rate_is_the_nearest_whole_fps() {
        assert_eq!(whole_fps(30, 1), Some(30));
        assert_eq!(
            whole_fps(10_000_000, 333_333),
            Some(30),
            "the 30 fps a UVC camera reports as a frame interval"
        );
        // The other intervals V4L2 drivers report, read as their reciprocal.
        assert_eq!(whole_fps(10_000_000, 333_334), Some(30));
        assert_eq!(whole_fps(10_000_000, 166_666), Some(60));
        assert_eq!(whole_fps(10_000_000, 400_000), Some(25));
        assert_eq!(whole_fps(10_000_000, 1_333_333), Some(8), "7.5 fps");
        assert_eq!(whole_fps(60_000, 1001), Some(60), "NTSC 59.94");
        assert_eq!(whole_fps(15, 2), Some(8), "7.5 rounds up");
        assert_eq!(whole_fps(29, 3), Some(10), "9.67");
    }

    #[test]
    fn a_rate_that_is_not_one_reads_as_none() {
        assert_eq!(whole_fps(30, 0), None, "no denominator");
        assert_eq!(whole_fps(0, 1), None);
        assert_eq!(whole_fps(1, 3), None, "a third of a frame per second");
        assert_eq!(
            whole_fps(1, 2),
            Some(1),
            "half a frame per second rounds up"
        );
    }

    #[test]
    fn extreme_ratios_neither_overflow_nor_wrap() {
        assert_eq!(whole_fps(u32::MAX, 1), Some(u32::MAX));
        assert_eq!(whole_fps(1, u32::MAX), None);
        assert_eq!(whole_fps(u32::MAX, u32::MAX), Some(1));
    }

    #[test]
    fn a_mode_reads_as_size_and_rate() {
        assert_eq!(mode(1920, 1080, 60).to_string(), "1920x1080 @ 60 fps");
    }
}
