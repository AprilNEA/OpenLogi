//! Flow's pointer and display queries on macOS, through typed Core Graphics
//! bindings. Global display points are the desktop space here, so the cursor
//! position needs no conversion.

use std::io;

use objc2_core_foundation::{CGPoint, CGRect};
use objc2_core_graphics::{
    CGAssociateMouseAndMouseCursorPosition, CGDirectDisplayID, CGDisplayBounds, CGError,
    CGEventFlags, CGEventSource, CGEventSourceStateID, CGGetActiveDisplayList,
    CGWarpMouseCursorPosition,
};
use tracing::debug;

use crate::{DesktopPoint, DisplayBounds, HookBackend, KeyModifiers, PointerState, WarpError};

/// The most displays one query reports. macOS supports far fewer.
const MAX_DISPLAYS: u32 = 32;

/// The macOS probe. Core Graphics needs no connection, so it holds nothing.
pub(crate) struct Probe;

impl Probe {
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the probe API is the same on every platform; X11 can fail to connect"
    )]
    pub(crate) fn connect() -> Option<Self> {
        Some(Self)
    }

    #[expect(
        clippy::unused_self,
        reason = "the probe API is the same on every platform; X11 needs its connection here"
    )]
    pub(crate) fn state(&mut self) -> Option<PointerState> {
        let position = <super::Backend as HookBackend>::cursor_position()?;
        let flags = CGEventSource::flags_state(CGEventSourceStateID::HIDSystemState);
        Some(PointerState {
            position: DesktopPoint {
                x: position.x,
                y: position.y,
            },
            modifiers: modifiers(flags),
        })
    }

    #[expect(
        clippy::unused_self,
        reason = "the probe API is the same on every platform; X11 needs its connection here"
    )]
    pub(crate) fn displays(&mut self) -> Vec<DisplayBounds> {
        let mut ids = [CGDirectDisplayID::default(); MAX_DISPLAYS as usize];
        let mut count = 0_u32;
        // SAFETY: `ids` has room for `MAX_DISPLAYS` ids and `count` is a live
        // `u32`; both outlive the call, which writes at most that many ids.
        let error =
            unsafe { CGGetActiveDisplayList(MAX_DISPLAYS, ids.as_mut_ptr(), &raw mut count) };
        if error != CGError::Success {
            debug!(code = error.0, "listing active displays failed");
            return Vec::new();
        }
        ids.iter()
            .take(count as usize)
            .map(|&id| bounds(CGDisplayBounds(id)))
            .collect()
    }

    #[expect(
        clippy::unused_self,
        reason = "the probe API is the same on every platform; X11 needs its connection here"
    )]
    pub(crate) fn warp(&mut self, to: DesktopPoint) -> Result<(), WarpError> {
        check(CGWarpMouseCursorPosition(CGPoint::new(to.x, to.y)))?;
        // Re-associate the mouse with the cursor so the next hardware motion
        // moves the cursor from the new position. Whether a warp alone ever
        // leaves them dissociated here is unverified; the call is harmless
        // when they already are associated.
        check(CGAssociateMouseAndMouseCursorPosition(true))
    }
}

fn modifiers(flags: CGEventFlags) -> KeyModifiers {
    KeyModifiers {
        shift: flags.contains(CGEventFlags::MaskShift),
        control: flags.contains(CGEventFlags::MaskControl),
        option: flags.contains(CGEventFlags::MaskAlternate),
        command: flags.contains(CGEventFlags::MaskCommand),
    }
}

fn bounds(rect: CGRect) -> DisplayBounds {
    DisplayBounds {
        x: rect.origin.x,
        y: rect.origin.y,
        width: rect.size.width,
        height: rect.size.height,
    }
}

fn check(error: CGError) -> Result<(), WarpError> {
    if error == CGError::Success {
        Ok(())
    } else {
        Err(WarpError::Rejected(io::Error::other(format!(
            "Core Graphics error {}",
            error.0
        ))))
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_graphics::CGEventFlags;

    use super::modifiers;
    use crate::KeyModifiers;

    #[test]
    fn each_modifier_flag_maps_to_its_own_key() {
        let flags = CGEventFlags::MaskShift | CGEventFlags::MaskCommand;

        assert_eq!(
            modifiers(flags),
            KeyModifiers {
                shift: true,
                control: false,
                option: false,
                command: true,
            }
        );
        assert_eq!(
            modifiers(CGEventFlags::MaskControl | CGEventFlags::MaskAlternate),
            KeyModifiers {
                shift: false,
                control: true,
                option: true,
                command: false,
            }
        );
        assert!(modifiers(CGEventFlags::empty()).is_empty());
    }
}
