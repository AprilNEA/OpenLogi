//! Continuous zoom output with one RAII terminal owner per gesture.

/// Lifecycle of a magnification gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoomPhase {
    /// First frame.
    Began,
    /// Subsequent motion.
    Changed,
    /// Normal zero-magnitude terminal frame.
    Ended,
    /// Interrupted zero-magnitude terminal frame.
    Cancelled,
}

/// Cursor and target captured by the runtime, retained for terminal cleanup.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoomContext {
    /// Global cursor x coordinate.
    pub x: f64,
    /// Global cursor y coordinate.
    pub y: f64,
    /// Captured target process, when the window lookup supplied one.
    pub process_id: Option<i32>,
    /// Captured native window identity, when known.
    pub window_id: Option<u64>,
}

/// Current cursor location; failure produces no gesture instead of anchoring at origin.
#[must_use]
pub fn cursor_location() -> Option<(f64, f64)> {
    #[cfg(target_os = "macos")]
    return crate::inject::macos::zoom::cursor_location();
    #[cfg(not(target_os = "macos"))]
    None
}

/// Foreground process captured for terminal cleanup. Does not activate an application.
#[must_use]
pub fn zoom_process_id() -> Option<i32> {
    #[cfg(target_os = "macos")]
    return crate::inject::macos::zoom::frontmost_process_id();
    #[cfg(not(target_os = "macos"))]
    None
}

/// Owner of a balanced native magnification gesture.
/// Dropping an unfinished gesture cancels it, including during unwinding.
pub struct ZoomGesture {
    context: ZoomContext,
    active: bool,
}

impl ZoomGesture {
    /// Begin output; invalid coordinates or event-allocation failure rejects the gesture.
    #[must_use]
    pub fn begin(context: ZoomContext, delta: f64) -> Option<Self> {
        if !delta.is_finite() || !context.x.is_finite() || !context.y.is_finite() {
            return None;
        }
        let mut gesture = Self {
            context,
            active: false,
        };
        if gesture.post(0.0, ZoomPhase::Began) {
            gesture.active = true;
            if delta != 0.0 {
                gesture.change(delta, (context.x, context.y));
            }
            Some(gesture)
        } else {
            None
        }
    }

    /// Emit signed fractional motion at the current cursor with the original target identity.
    pub fn change(&mut self, delta: f64, cursor: (f64, f64)) {
        if !self.active || !delta.is_finite() || !cursor.0.is_finite() || !cursor.1.is_finite() {
            return;
        }
        self.context.x = cursor.0;
        self.context.y = cursor.1;
        self.post(delta, ZoomPhase::Changed);
    }

    /// Finish once. Terminal frames always bypass any smoothing and carry zero motion.
    pub fn finish(&mut self, phase: ZoomPhase) {
        if !matches!(phase, ZoomPhase::Ended | ZoomPhase::Cancelled) || !self.active {
            return;
        }
        self.active = false;
        self.post(0.0, phase);
    }

    fn post(&self, delta: f64, phase: ZoomPhase) -> bool {
        #[cfg(target_os = "macos")]
        return crate::inject::macos::zoom::post(self.context, delta, phase);
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (delta, phase, self.context);
            false
        }
    }
}

impl Drop for ZoomGesture {
    fn drop(&mut self) {
        self.finish(ZoomPhase::Cancelled);
    }
}
