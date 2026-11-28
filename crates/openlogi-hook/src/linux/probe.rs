//! Native desktop coordinates from X11 or the GNOME Wayland companion.
//! XWayland cannot see the pointer over native clients or move the seat.

mod gnome;
mod x11;

use super::foreground::{SessionKind, detect_session_kind};
use crate::{DesktopPoint, DisplayBounds, PointerState, WarpError};

pub(crate) enum Probe {
    X11(Box<x11::Probe>),
    Gnome(gnome::Probe),
}

impl Probe {
    pub(crate) fn connect() -> Option<Self> {
        if detect_session_kind() == SessionKind::Wayland {
            // The extension can appear after login or after the agent starts.
            // Only the GNOME backend can supply compositor-wide coordinates.
            let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
            if desktop.is_empty()
                || desktop
                    .split(':')
                    .any(|part| part.eq_ignore_ascii_case("GNOME"))
            {
                return Some(Self::Gnome(gnome::Probe::default()));
            }
            return None;
        }
        x11::Probe::connect().map(Box::new).map(Self::X11)
    }

    pub(crate) fn state(&mut self) -> Option<PointerState> {
        match self {
            Self::X11(p) => p.state(),
            Self::Gnome(p) => p.state(),
        }
    }

    pub(crate) fn displays(&mut self) -> Vec<DisplayBounds> {
        match self {
            Self::X11(p) => p.displays(),
            Self::Gnome(p) => p.displays(),
        }
    }

    pub(crate) fn generation(&self) -> Option<&str> {
        match self {
            Self::X11(_) => None,
            Self::Gnome(p) => Some(p.generation()),
        }
    }

    pub(crate) fn warp(&mut self, to: DesktopPoint) -> Result<(), WarpError> {
        match self {
            Self::X11(p) => p.warp(to),
            Self::Gnome(p) => p.warp(to),
        }
    }
}
