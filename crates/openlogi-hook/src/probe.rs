//! Pointer and display-geometry queries for Flow's pointer thread.
//!
//! [`PointerProbe`] reads the cursor, the held modifier keys and the display
//! bounds, and moves the cursor. All of them share the platform's native
//! desktop coordinate space, in which the displays tile without overlap:
//! global display points on macOS, root-window pixels on X11, and the
//! virtual-screen coordinates that Win32 reports to this process on Windows.
//!
//! On Windows this space differs from [`CursorPosition`](crate::CursorPosition),
//! which divides by each monitor's DPI scale to match GPUI. Two displays with
//! different scale factors can overlap after that division, so edge geometry
//! cannot use it.

use thiserror::Error;

use crate::KeyModifiers;

#[cfg(target_os = "linux")]
use crate::linux::probe as platform;
#[cfg(target_os = "macos")]
use crate::macos::probe as platform;
#[cfg(target_os = "windows")]
use crate::windows::probe as platform;

/// A point in the native desktop coordinate space (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DesktopPoint {
    /// Horizontal coordinate, increasing rightward.
    pub x: f64,
    /// Vertical coordinate, increasing downward.
    pub y: f64,
}

/// One display's bounds in the native desktop coordinate space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayBounds {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width, extending rightward from `x`.
    pub width: f64,
    /// Height, extending downward from `y`.
    pub height: f64,
}

impl DisplayBounds {
    /// Whether `point` lies on this display. The left and top edges are
    /// inside; the right and bottom edges belong to the next display.
    #[must_use]
    pub fn contains(&self, point: DesktopPoint) -> bool {
        point.x >= self.x
            && point.x < self.x + self.width
            && point.y >= self.y
            && point.y < self.y + self.height
    }
}

/// The cursor position and the held modifier keys, read together.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerState {
    /// Where the cursor is.
    pub position: DesktopPoint,
    /// Which modifier keys are held.
    pub modifiers: KeyModifiers,
}

/// Why [`PointerProbe::warp`] did not move the cursor.
#[derive(Debug, Error)]
pub enum WarpError {
    /// No display contains the target, so the window system would clamp the
    /// cursor somewhere else.
    #[error("no display contains the warp target")]
    OutsideDisplays,
    /// The window system rejected the request.
    #[error("the window system rejected the cursor warp")]
    Rejected(#[source] std::io::Error),
}

/// Blocking pointer and display queries, owned by one thread.
///
/// A call can wait for a window-server round trip. Call it from a dedicated
/// thread, never from a [`Hook`](crate::Hook) callback or an async task.
pub struct PointerProbe {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    inner: platform::Probe,
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    never: std::convert::Infallible,
}

impl PointerProbe {
    /// Open the probe for the current desktop session.
    ///
    /// `None` when this session cannot report or move the cursor: a native
    /// Wayland session, an X11 session whose display is unreachable, or a
    /// platform without a backend.
    #[must_use]
    pub fn connect() -> Option<Self> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            platform::Probe::connect().map(|inner| Self { inner })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            None
        }
    }

    /// Read the cursor position and the held modifiers. `None` when the window
    /// system does not answer.
    pub fn state(&mut self) -> Option<PointerState> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            self.inner.state()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            match self.never {}
        }
    }

    /// Read the bounds of every active display. Empty when the window system
    /// does not answer.
    pub fn displays(&mut self) -> Vec<DisplayBounds> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            self.inner.displays()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            match self.never {}
        }
    }

    /// Move the cursor to `to`, which must lie on an active display.
    ///
    /// # Errors
    ///
    /// [`WarpError::OutsideDisplays`] when no active display contains `to`, and
    /// [`WarpError::Rejected`] when the window system refuses the move.
    pub fn warp(&mut self, to: DesktopPoint) -> Result<(), WarpError> {
        if !self.displays().iter().any(|display| display.contains(to)) {
            return Err(WarpError::OutsideDisplays);
        }
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            self.inner.warp(to)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            match self.never {}
        }
    }
}

#[cfg(test)]
mod tests;
