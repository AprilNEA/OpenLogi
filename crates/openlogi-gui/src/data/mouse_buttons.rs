//! Mouse-model hotspot geometry. Hotspot bounds are authored in mouse-model-
//! local pixels (the SVG canvas is 420×560 — see [`MOUSE_MODEL_SIZE`]) and
//! stored as plain `f32` tuples so this module stays purely data and doesn't
//! drag in `gpui` types.
//!
//! Button identifiers and the action vocabulary live in
//! [`openlogi_core::binding`]; this module re-exports them so existing call
//! sites can keep importing from the GUI crate without churn.

#![allow(
    dead_code,
    reason = "scaffolding consumed by UI.md phases 3–6 (carousel, popover, hotspots)"
)]

pub use openlogi_core::binding::{
    Action, Binding, ButtonId, Category, GestureDirection, default_binding, default_gesture_binding,
};

/// One visual target in the mouse diagram.
///
/// Most targets correspond to one physical button. Thumb-wheel rotation is a
/// single visual target backed by two directional bindings, so it has its own
/// identity rather than pretending to be either direction or the wheel click.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum MouseControlId {
    Button(ButtonId),
    ThumbwheelRotation,
}

impl MouseControlId {
    /// Return the physical button when this target represents one.
    #[must_use]
    pub(crate) const fn button(self) -> Option<ButtonId> {
        match self {
            Self::Button(button) => Some(button),
            Self::ThumbwheelRotation => None,
        }
    }

    /// Collapse either live thumb-wheel direction into the one diagram target.
    #[must_use]
    pub(crate) const fn from_active_button(button: ButtonId) -> Self {
        match button {
            ButtonId::ThumbwheelScrollUp | ButtonId::ThumbwheelScrollDown => {
                Self::ThumbwheelRotation
            }
            _ => Self::Button(button),
        }
    }

    #[must_use]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Button(button) => button.label(),
            Self::ThumbwheelRotation => "Thumb Wheel",
        }
    }
}

impl From<ButtonId> for MouseControlId {
    fn from(button: ButtonId) -> Self {
        Self::Button(button)
    }
}

/// The size of the mouse model canvas. Hotspot coords are relative to this.
pub const MOUSE_MODEL_SIZE: (f32, f32) = (420., 560.);

/// Hotspot rectangle in mouse-model-local coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hotspot {
    pub(crate) id: MouseControlId,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Hotspot {
    /// Returns the center point — convenient for leader lines.
    #[inline]
    #[must_use]
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }
}

/// Fallback hotspot layout for the no-asset path (synthetic silhouette).
/// Primary L/R click are intentionally absent — Logi doesn't expose them
/// as remappable and we follow the same rule everywhere.
#[must_use]
pub fn default_hotspots(thumbwheel: bool) -> Vec<Hotspot> {
    let mut hotspots = vec![
        Hotspot {
            id: ButtonId::MiddleClick.into(),
            x: 180.,
            y: 110.,
            w: 60.,
            h: 90.,
        },
        Hotspot {
            id: ButtonId::Back.into(),
            x: 0.,
            y: 220.,
            w: 40.,
            h: 60.,
        },
        Hotspot {
            id: ButtonId::Forward.into(),
            x: 0.,
            y: 290.,
            w: 40.,
            h: 60.,
        },
        Hotspot {
            id: ButtonId::DpiToggle.into(),
            x: 175.,
            y: 230.,
            w: 70.,
            h: 40.,
        },
        Hotspot {
            id: ButtonId::GestureButton.into(),
            x: 8.,
            y: 380.,
            w: 44.,
            h: 80.,
        },
    ];
    if thumbwheel {
        hotspots.push(Hotspot {
            id: MouseControlId::ThumbwheelRotation,
            x: 8.,
            y: 140.,
            w: 44.,
            h: 70.,
        });
    }
    hotspots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_thumbwheel_directions_share_one_control() {
        assert_eq!(
            MouseControlId::from_active_button(ButtonId::ThumbwheelScrollUp),
            MouseControlId::ThumbwheelRotation
        );
        assert_eq!(
            MouseControlId::from_active_button(ButtonId::ThumbwheelScrollDown),
            MouseControlId::ThumbwheelRotation
        );
    }

    #[test]
    fn fallback_thumbwheel_is_capability_gated() {
        assert!(
            !default_hotspots(false)
                .iter()
                .any(|hotspot| { hotspot.id == MouseControlId::ThumbwheelRotation })
        );
        assert_eq!(
            default_hotspots(true)
                .iter()
                .filter(|hotspot| hotspot.id == MouseControlId::ThumbwheelRotation)
                .count(),
            1
        );
    }

    #[test]
    fn default_hotspots_expose_the_gesture_button() {
        let hotspots = default_hotspots(false);
        assert!(
            hotspots
                .iter()
                .any(|h| { h.id == MouseControlId::Button(ButtonId::GestureButton) }),
            "the gesture button must be a mappable hotspot in the synthetic model"
        );
    }

    #[test]
    fn default_hotspots_omit_primary_clicks() {
        let hotspots = default_hotspots(false);
        assert!(
            !hotspots.iter().any(|h| {
                matches!(
                    h.id,
                    MouseControlId::Button(ButtonId::LeftClick | ButtonId::RightClick)
                )
            }),
            "primary clicks are not remappable and must stay out of the model"
        );
    }
}
