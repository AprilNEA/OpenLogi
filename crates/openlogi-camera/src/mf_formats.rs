//! Which stream modes a Media Foundation camera's native media types offer,
//! and which native type a mode needs.
//!
//! A source reader lists one native media type per frame size, frame rate
//! *and* pixel format: a camera that streams 720p at 30 and 60 fps in both
//! NV12 and YUY2 lists four. This module turns that list into the
//! platform-independent [`StreamModes`], and back: for a chosen mode, which
//! native type to select. Plain data in and out, so the decisions are testable
//! without a camera; reading the list and selecting the pick is
//! `capture_windows.rs`.
//!
//! The list it is handed is already limited to the pixel formats the reader
//! converts to RGB32 (the others would fail to open), so a mode offered here
//! is one the preview can stream.

use crate::{StreamMode, StreamModes};

/// One native media type, reduced to what a mode names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativeFormat {
    /// The type's index for the reader's `GetNativeMediaType`, which is how
    /// the one chosen is found again to select it.
    pub index: u32,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Whole frames per second ([`crate::stream_mode::whole_fps`]); `0` when
    /// the type names no rate, which makes it no mode.
    pub fps: u32,
}

impl NativeFormat {
    /// The mode this type is.
    const fn mode(self) -> StreamMode {
        StreamMode {
            width: self.width,
            height: self.height,
            fps: self.fps,
        }
    }
}

/// Every mode `formats` offer. Types of one size and rate in different pixel
/// formats are one mode; empty sizes and rates are dropped by
/// [`StreamModes::new`].
pub(crate) fn offered(formats: &[NativeFormat]) -> StreamModes {
    StreamModes::new(formats.iter().map(|format| format.mode()))
}

/// The native type to select to stream at `requested`: one offering that mode
/// when the camera has it, else one offering the camera's default
/// ([`StreamModes::resolve`]). `None` when the camera offers no mode at all.
///
/// When several types offer the mode, the first the driver lists wins, so the
/// pick is the same on every open.
pub(crate) fn choose(
    formats: &[NativeFormat],
    requested: Option<StreamMode>,
) -> Option<NativeFormat> {
    let mode = offered(formats).resolve(requested)?;
    formats.iter().copied().find(|format| format.mode() == mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn mode(width: u32, height: u32, fps: u32) -> StreamMode {
        StreamMode { width, height, fps }
    }

    /// The types a reader lists, in order, as `(width, height, fps)`.
    fn listed(types: &[(u32, u32, u32)]) -> Vec<NativeFormat> {
        types
            .iter()
            .zip(0..)
            .map(|(&(width, height, fps), index)| NativeFormat {
                index,
                width,
                height,
                fps,
            })
            .collect()
    }

    #[test]
    fn one_size_and_rate_in_two_pixel_formats_is_one_mode() {
        let formats = listed(&[
            (1280, 720, 30), // NV12
            (1280, 720, 30), // YUY2
            (1280, 720, 60),
        ]);
        assert_eq!(
            offered(&formats).iter().collect::<Vec<_>>(),
            [mode(1280, 720, 60), mode(1280, 720, 30)]
        );
    }

    #[test]
    fn a_type_with_no_rate_or_no_size_is_no_mode() {
        let formats = listed(&[(1280, 720, 0), (0, 0, 30)]);
        assert!(offered(&formats).is_empty());
        assert_eq!(choose(&formats, None), None);
    }

    #[test]
    fn choosing_finds_the_type_that_offers_the_mode() {
        let formats = listed(&[(640, 480, 30), (1920, 1080, 5), (1920, 1080, 30)]);
        let chosen = choose(&formats, Some(mode(1920, 1080, 30))).unwrap();
        assert_eq!(chosen.index, 2);
        assert_eq!(chosen.mode(), mode(1920, 1080, 30));
    }

    #[test]
    fn the_first_type_offering_a_mode_wins() {
        let formats = listed(&[(1280, 720, 30), (1280, 720, 30)]);
        assert_eq!(
            choose(&formats, Some(mode(1280, 720, 30))).unwrap().index,
            0
        );
    }

    #[test]
    fn a_request_the_camera_does_not_offer_opens_at_its_default() {
        // A USB 2.0 link: 1080p only at 5 fps, and a 720p that runs at 10.
        let formats = listed(&[(1920, 1080, 5), (1280, 720, 10), (640, 480, 30)]);
        let default = offered(&formats).default_mode().unwrap();

        assert_eq!(default, mode(1280, 720, 10));
        // 1080p at 30 is not on offer at this link speed.
        let chosen = choose(&formats, Some(mode(1920, 1080, 30))).unwrap();
        assert_eq!(chosen.mode(), default);
        assert_eq!(choose(&formats, None).unwrap().mode(), default);
    }

    #[test]
    fn a_camera_offering_nothing_has_nothing_to_choose() {
        assert!(offered(&[]).is_empty());
        assert_eq!(choose(&[], Some(mode(1280, 720, 30))), None);
    }
}
