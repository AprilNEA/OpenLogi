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
    Action, ButtonId, Category, GestureDirection, default_binding, default_gesture_binding,
};

/// The size of the mouse model canvas. Hotspot coords are relative to this.
pub const MOUSE_MODEL_SIZE: (f32, f32) = (420., 560.);

/// Hotspot rectangle in mouse-model-local coordinates.
#[derive(Clone, Copy, Debug)]
pub struct Hotspot {
    pub id: ButtonId,
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
pub fn default_hotspots() -> Vec<Hotspot> {
    vec![
        Hotspot {
            id: ButtonId::MiddleClick,
            x: 180.,
            y: 110.,
            w: 60.,
            h: 90.,
        },
        Hotspot {
            id: ButtonId::Back,
            x: 0.,
            y: 220.,
            w: 40.,
            h: 60.,
        },
        Hotspot {
            id: ButtonId::Forward,
            x: 0.,
            y: 290.,
            w: 40.,
            h: 60.,
        },
        Hotspot {
            id: ButtonId::DpiToggle,
            x: 175.,
            y: 230.,
            w: 70.,
            h: 40.,
        },
        Hotspot {
            id: ButtonId::GestureButton,
            x: 8.,
            y: 380.,
            w: 44.,
            h: 80.,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_hotspots_expose_the_gesture_button() {
        let hotspots = default_hotspots();
        assert!(
            hotspots
                .iter()
                .any(|h| matches!(h.id, ButtonId::GestureButton)),
            "the gesture button must be a mappable hotspot in the synthetic model"
        );
    }

    #[test]
    fn default_hotspots_omit_primary_clicks() {
        let hotspots = default_hotspots();
        assert!(
            !hotspots
                .iter()
                .any(|h| matches!(h.id, ButtonId::LeftClick | ButtonId::RightClick)),
            "primary clicks are not remappable and must stay out of the model"
        );
    }
}
