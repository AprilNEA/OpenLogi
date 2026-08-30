//! Sans-I/O hold-mode pan and zoom session state.
//!
//! The agent calls `process::exit` in places, so a `Drop` impl cannot be the
//! terminal path. Callers must drive [`GestureSessions::flush`] (via
//! [`super::flush_gesture_sessions`]) on every teardown, and
//! [`GestureSessions::seal`] on the way out of the process. A late pan after
//! `end` must not reopen the session; zoom continuous reopens by contract,
//! which is why the exit path seals rather than only flushing.

use openlogi_core::scroll::ScrollDelta;

use super::{QuantizedScroll, ScrollQuantizer};

/// Trackpad-style scroll / magnify phase bits (`CGScrollPhase` /
/// `CGGesturePhase`). These are **not** `NSEventPhase` values: AppKit's
/// Changed is `1 << 2` (4), while the CG fields use 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GesturePhase {
    Began,
    Changed,
    Ended,
}

#[cfg(target_os = "macos")]
impl GesturePhase {
    /// `kCGScrollPhase*` / `kCGGesturePhase*` from the macOS SDK
    /// `CGEventTypes.h` (`Began = 1`, `Changed = 2`, `Ended = 4`). These
    /// are not `NSEventPhase` values: AppKit's Changed is `1 << 2` (4).
    pub(super) const fn cg_phase_bits(self) -> i64 {
        match self {
            Self::Began => 1,
            Self::Changed => 2,
            Self::Ended => 4,
        }
    }
}

/// One pixel-unit pan report in **screen** space: +x right, +y down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PanFrame {
    pub phase: GesturePhase,
    pub dx: i32,
    pub dy: i32,
}

/// One continuous-magnification report. Positive `amount` zooms in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ZoomFrame {
    pub phase: GesturePhase,
    pub amount: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Live {
    #[default]
    Closed,
    Open,
}

/// Process-global pair of hold-mode gesture sessions.
#[derive(Default)]
pub(super) struct GestureSessions {
    pan: Live,
    pan_pixels: ScrollQuantizer,
    zoom: Live,
    /// Set once by [`Self::seal`], never cleared. Closing a session is not
    /// enough on the way out: `zoom_continuous` opens a pinch from `Closed`
    /// by contract, so a watcher thread still streaming during teardown would
    /// reopen one after the final flush, and `process::exit` skips [`Drop`].
    /// Off macOS that leaves a held Ctrl with nothing to release it.
    sealed: bool,
}

impl GestureSessions {
    /// Open a pan session. A second begin while already open is a no-op so
    /// the OS never sees `began` twice without an `ended`.
    pub(super) fn begin_pan(&mut self) -> Option<PanFrame> {
        if self.sealed || self.pan == Live::Open {
            return None;
        }
        self.pan = Live::Open;
        self.pan_pixels = ScrollQuantizer::default();
        Some(PanFrame {
            phase: GesturePhase::Began,
            dx: 0,
            dy: 0,
        })
    }

    /// Stream one pan report. Ignored unless a session is open — a torn-down
    /// hold must stay terminal.
    pub(super) fn pan(&mut self, dx: f32, dy: f32) -> Option<PanFrame> {
        if self.pan != Live::Open {
            return None;
        }
        if !dx.is_finite() || !dy.is_finite() {
            return None;
        }
        let QuantizedScroll { x, y } = self
            .pan_pixels
            .quantize(ScrollDelta::pixels(f64::from(dx), f64::from(dy)), 1.0);
        if x == 0 && y == 0 {
            return None;
        }
        Some(PanFrame {
            phase: GesturePhase::Changed,
            dx: x,
            dy: y,
        })
    }

    /// Close the pan session. Idempotent.
    pub(super) fn end_pan(&mut self) -> Option<PanFrame> {
        if self.pan != Live::Open {
            return None;
        }
        self.pan = Live::Closed;
        self.pan_pixels = ScrollQuantizer::default();
        Some(PanFrame {
            phase: GesturePhase::Ended,
            dx: 0,
            dy: 0,
        })
    }

    /// Stream one magnification increment, opening a pinch if needed.
    pub(super) fn zoom_continuous(&mut self, amount: f32) -> Option<ZoomFrame> {
        if self.sealed || !amount.is_finite() {
            return None;
        }
        match self.zoom {
            Live::Closed => {
                self.zoom = Live::Open;
                Some(ZoomFrame {
                    phase: GesturePhase::Began,
                    amount,
                })
            }
            Live::Open => Some(ZoomFrame {
                phase: GesturePhase::Changed,
                amount,
            }),
        }
    }

    /// Close an open pinch immediately. Idempotent.
    pub(super) fn end_zoom(&mut self) -> Option<ZoomFrame> {
        if self.zoom != Live::Open {
            return None;
        }
        self.zoom = Live::Closed;
        Some(ZoomFrame {
            phase: GesturePhase::Ended,
            amount: 0.0,
        })
    }

    /// Close every open session. Safe to call when both are already closed.
    /// A later press may still open a new one.
    pub(super) fn flush(&mut self) -> FlushFrames {
        FlushFrames {
            pan: self.end_pan(),
            zoom: self.end_zoom(),
        }
    }

    /// Close every open session and refuse to open another. Terminal, for the
    /// last moment before `process::exit` or `exec`.
    pub(super) fn seal(&mut self) -> FlushFrames {
        let frames = self.flush();
        self.sealed = true;
        frames
    }
}

/// Frames produced by one [`GestureSessions::flush`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct FlushFrames {
    pub pan: Option<PanFrame>,
    pub zoom: Option<ZoomFrame>,
}

/// Invert screen-space pan (+x right, +y down) into [`ScrollDelta`] pixels
/// (+x scrolls right, +y scrolls up) so content follows the hand on both
/// axes. Mouse-right is therefore scroll-left; mouse-down is scroll-down.
pub(super) fn scroll_pixels_from_screen_pan(dx: i32, dy: i32) -> (i32, i32) {
    (dx.saturating_neg(), dy.saturating_neg())
}

/// Line/point relationship carried by native macOS continuous scroll events,
/// reused so Linux/Windows wheel-tick pan stays the same physical scale.
#[cfg(any(test, target_os = "linux", target_os = "windows"))]
pub(super) const POINTS_PER_WHEEL_TICK: f64 = 10.0;

/// Convert screen-space integer pixels to wheel ticks for platforms that
/// cannot emit pixel-unit scroll.
#[cfg(any(test, target_os = "linux", target_os = "windows"))]
pub(super) fn wheel_ticks_from_screen_pixels(dx: i32, dy: i32) -> (f64, f64) {
    let (sx, sy) = scroll_pixels_from_screen_pan(dx, dy);
    (
        f64::from(sx) / POINTS_PER_WHEEL_TICK,
        f64::from(sy) / POINTS_PER_WHEEL_TICK,
    )
}

/// Banks magnification increments into whole wheel detents for Linux/Windows
/// Ctrl+wheel zoom. One detent is [`MAGNIFICATION_PER_WHEEL_DETENT`].
#[cfg(any(test, target_os = "linux", target_os = "windows"))]
#[derive(Default)]
pub(super) struct WheelDetentBank {
    inner: ScrollQuantizer,
}

/// Magnification increment that equals one Ctrl+wheel detent. Chosen to match
/// a typical browser step (~10% per notch), not a tautology of the emitter.
#[cfg(any(test, target_os = "linux", target_os = "windows"))]
pub(super) const MAGNIFICATION_PER_WHEEL_DETENT: f64 = 0.1;

#[cfg(any(test, target_os = "linux", target_os = "windows"))]
impl WheelDetentBank {
    /// Absorb one increment and return the signed whole-detent count to emit.
    pub(super) fn ingest(&mut self, amount: f32) -> i32 {
        if !amount.is_finite() {
            return 0;
        }
        self.inner
            .quantize(
                ScrollDelta::wheel_ticks(0.0, f64::from(amount)),
                1.0 / MAGNIFICATION_PER_WHEEL_DETENT,
            )
            .y
    }

    pub(super) fn reset(&mut self) {
        self.inner = ScrollQuantizer::default();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GesturePhase, GestureSessions, Live, PanFrame, WheelDetentBank, ZoomFrame,
        scroll_pixels_from_screen_pan, wheel_ticks_from_screen_pixels,
    };

    #[cfg(target_os = "macos")]
    #[test]
    fn gesture_phase_uses_cg_bits_not_nsevent_phase() {
        assert_eq!(GesturePhase::Began.cg_phase_bits(), 1);
        assert_eq!(GesturePhase::Changed.cg_phase_bits(), 2);
        assert_eq!(GesturePhase::Ended.cg_phase_bits(), 4);
    }

    #[test]
    fn double_begin_does_not_emit_a_second_began() {
        let mut sessions = GestureSessions::default();
        assert_eq!(
            sessions.begin_pan(),
            Some(PanFrame {
                phase: GesturePhase::Began,
                dx: 0,
                dy: 0
            })
        );
        assert_eq!(sessions.begin_pan(), None);
        assert_eq!(sessions.pan, Live::Open);
    }

    #[test]
    fn late_pan_after_end_does_not_reopen() {
        let mut sessions = GestureSessions::default();
        sessions.begin_pan();
        assert_eq!(
            sessions.end_pan(),
            Some(PanFrame {
                phase: GesturePhase::Ended,
                dx: 0,
                dy: 0
            })
        );
        assert_eq!(sessions.pan(12.0, -4.0), None);
        assert_eq!(sessions.end_pan(), None);
        assert_eq!(sessions.pan, Live::Closed);
    }

    #[test]
    fn fractional_pan_banks_until_nearest_pixel() {
        let mut sessions = GestureSessions::default();
        sessions.begin_pan();
        // 0.4 is closer to 0 than 1; a second 0.4 crosses the midpoint.
        assert_eq!(sessions.pan(0.4, 0.0), None);
        assert_eq!(
            sessions.pan(0.4, 0.0),
            Some(PanFrame {
                phase: GesturePhase::Changed,
                dx: 1,
                dy: 0
            })
        );
    }

    #[test]
    fn pan_keeps_screen_down_positive_until_the_scroll_seam() {
        let mut sessions = GestureSessions::default();
        sessions.begin_pan();
        assert_eq!(
            sessions.pan(3.0, 5.0),
            Some(PanFrame {
                phase: GesturePhase::Changed,
                dx: 3,
                dy: 5
            })
        );
    }

    // Content-follows-hand pan: invert each screen axis independently into
    // `ScrollDelta` space (+x scrolls right, +y scrolls up). A combined
    // (dx, dy) assertion hid the live bug where only Y was inverted.

    #[test]
    fn mouse_right_scrolls_left_so_content_follows() {
        assert_eq!(scroll_pixels_from_screen_pan(7, 0), (-7, 0));
    }

    #[test]
    fn mouse_left_scrolls_right_so_content_follows() {
        assert_eq!(scroll_pixels_from_screen_pan(-7, 0), (7, 0));
    }

    #[test]
    fn mouse_down_scrolls_down_so_content_follows() {
        assert_eq!(scroll_pixels_from_screen_pan(0, 7), (0, -7));
    }

    #[test]
    fn mouse_up_scrolls_up_so_content_follows() {
        assert_eq!(scroll_pixels_from_screen_pan(0, -7), (0, 7));
    }

    #[test]
    fn screen_min_saturates_per_axis() {
        assert_eq!(scroll_pixels_from_screen_pan(i32::MIN, 0), (i32::MAX, 0));
        assert_eq!(scroll_pixels_from_screen_pan(0, i32::MIN), (0, i32::MAX));
    }

    #[test]
    fn wheel_tick_horizontal_matches_pixel_sign() {
        assert_eq!(wheel_ticks_from_screen_pixels(10, 0), (-1.0, 0.0));
        assert_eq!(wheel_ticks_from_screen_pixels(-10, 0), (1.0, 0.0));
        assert_eq!(wheel_ticks_from_screen_pixels(-5, 0), (0.5, 0.0));
    }

    #[test]
    fn wheel_tick_vertical_matches_pixel_sign() {
        assert_eq!(wheel_ticks_from_screen_pixels(0, 10), (0.0, -1.0));
        assert_eq!(wheel_ticks_from_screen_pixels(0, -10), (0.0, 1.0));
    }

    #[test]
    fn zoom_begins_on_the_first_delta_and_changes_after() {
        let mut sessions = GestureSessions::default();
        assert_eq!(
            sessions.zoom_continuous(0.02),
            Some(ZoomFrame {
                phase: GesturePhase::Began,
                amount: 0.02
            })
        );
        assert_eq!(
            sessions.zoom_continuous(-0.01),
            Some(ZoomFrame {
                phase: GesturePhase::Changed,
                amount: -0.01
            })
        );
        assert_eq!(
            sessions.end_zoom(),
            Some(ZoomFrame {
                phase: GesturePhase::Ended,
                amount: 0.0
            })
        );
        assert_eq!(sessions.end_zoom(), None);
    }

    #[test]
    fn zoom_continuous_reopens_after_end() {
        let mut sessions = GestureSessions::default();
        sessions.zoom_continuous(0.01);
        sessions.end_zoom();
        assert_eq!(
            sessions.zoom_continuous(0.03),
            Some(ZoomFrame {
                phase: GesturePhase::Began,
                amount: 0.03
            })
        );
    }

    #[test]
    fn non_finite_input_does_not_open_or_move_a_session() {
        let mut sessions = GestureSessions::default();
        assert_eq!(sessions.pan(1.0, 1.0), None);
        assert_eq!(sessions.zoom_continuous(f32::NAN), None);
        assert_eq!(sessions.zoom, Live::Closed);
        sessions.begin_pan();
        assert_eq!(sessions.pan(f32::INFINITY, 0.0), None);
        assert_eq!(sessions.pan, Live::Open);
    }

    #[test]
    fn flush_ends_open_sessions_once() {
        let mut sessions = GestureSessions::default();
        sessions.begin_pan();
        sessions.zoom_continuous(0.05);
        let first = sessions.flush();
        assert_eq!(first.pan.map(|f| f.phase), Some(GesturePhase::Ended));
        assert_eq!(first.zoom.map(|f| f.phase), Some(GesturePhase::Ended));
        let second = sessions.flush();
        assert_eq!(second.pan, None);
        assert_eq!(second.zoom, None);
        assert_eq!(sessions.pan(8.0, 0.0), None);
    }

    #[test]
    fn a_sealed_session_cannot_be_reopened_by_a_late_watcher() {
        let mut sessions = GestureSessions::default();
        sessions.zoom_continuous(0.05);
        sessions.begin_pan();
        let frames = sessions.seal();
        assert_eq!(frames.pan.map(|f| f.phase), Some(GesturePhase::Ended));
        assert_eq!(frames.zoom.map(|f| f.phase), Some(GesturePhase::Ended));
        assert_eq!(
            sessions.zoom_continuous(0.05),
            None,
            "zoom opens a pinch from closed by contract, which is exactly what \
             must not happen after the exit flush"
        );
        assert_eq!(sessions.begin_pan(), None);
        assert_eq!(sessions.pan(8.0, 0.0), None);
    }

    #[test]
    fn a_plain_flush_still_lets_the_next_hold_open() {
        let mut sessions = GestureSessions::default();
        sessions.begin_pan();
        sessions.flush();
        assert_eq!(
            sessions.begin_pan(),
            Some(PanFrame {
                phase: GesturePhase::Began,
                dx: 0,
                dy: 0
            }),
            "only the exit path is terminal"
        );
    }

    #[test]
    fn detent_bank_emits_only_after_crossing_a_notch() {
        let mut bank = WheelDetentBank::default();
        // 0.04 magnification is 0.4 of a detent — below the rounding midpoint.
        // A second 0.04 crosses it. Opposite motion pays the residual back.
        assert_eq!(bank.ingest(0.04), 0);
        assert_eq!(bank.ingest(0.04), 1);
        assert_eq!(bank.ingest(-0.08), -1);
        bank.reset();
        assert_eq!(bank.ingest(0.04), 0);
    }
}
