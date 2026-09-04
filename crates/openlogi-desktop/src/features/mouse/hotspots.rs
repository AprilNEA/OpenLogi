//! Mouse hotspot geometry. Bounds are authored in model-local pixels (the
//! SVG canvas is 420×560 — see [`MOUSE_MODEL_SIZE`]) and
//! stored as plain `f32` tuples so this module stays purely data and doesn't
//! drag in `gpui` types.

use openlogi_core::binding::ButtonId;

/// One visual target in the mouse diagram.
///
/// Most targets correspond to one physical button. Thumb-wheel rotation is a
/// single visual target backed by two directional bindings, so it has its own
/// identity rather than pretending to be either direction or the wheel click.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::From)]
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
    pub(crate) fn translation_key(self) -> &'static str {
        match self {
            Self::Button(button) => button.translation_key(),
            Self::ThumbwheelRotation => "pointer.thumb_wheel",
        }
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

/// Which optional controls the synthetic silhouette should offer, derived
/// from the device's measured [`openlogi_core::device::Capabilities`]. A
/// plain `bool` parameter stopped being enough once a second independent
/// fact joined `thumbwheel` — see `.claude/rules/rust.md` on boolean-blind
/// parameters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FallbackControls {
    /// A horizontal thumb wheel is available — mirrors `Capabilities::thumbwheel`.
    pub thumbwheel: bool,
    /// The device reports HID++ `0x8100`+`0x8110` (a G-series gaming mouse
    /// captured through the spy tap, not `0x1b04`). These devices have
    /// neither a "ModeShift" DPI-toggle button nor a dedicated MX-style
    /// gesture button — [`ButtonId::DpiToggle`] and [`ButtonId::GestureButton`]
    /// are dropped from the silhouette in favor of the gaming-only buttons.
    pub gaming_buttons: bool,
}

/// Fallback hotspot layout for the no-asset path (synthetic silhouette).
/// Primary L/R click are intentionally absent — Logi doesn't expose them
/// as remappable and we follow the same rule everywhere.
#[must_use]
pub fn default_hotspots(controls: FallbackControls) -> Vec<Hotspot> {
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
    ];
    if controls.gaming_buttons {
        // Approximate placement — this is generic silhouette art, not a claim
        // about this specific model's shape. A thumb paddle for the DPI Shift
        // modifier, and a cluster behind the wheel for the profile/DPI trio,
        // mirroring where these controls sit on a G502 X PLUS.
        hotspots.push(Hotspot {
            id: ButtonId::DpiShift.into(),
            x: 8.,
            y: 340.,
            w: 44.,
            h: 50.,
        });
        hotspots.push(Hotspot {
            id: ButtonId::ProfileCycle.into(),
            x: 175.,
            y: 230.,
            w: 70.,
            h: 26.,
        });
        hotspots.push(Hotspot {
            id: ButtonId::DpiUp.into(),
            x: 175.,
            y: 258.,
            w: 34.,
            h: 26.,
        });
        hotspots.push(Hotspot {
            id: ButtonId::DpiDown.into(),
            x: 211.,
            y: 258.,
            w: 34.,
            h: 26.,
        });
    } else {
        hotspots.push(Hotspot {
            id: ButtonId::DpiToggle.into(),
            x: 175.,
            y: 230.,
            w: 70.,
            h: 40.,
        });
        hotspots.push(Hotspot {
            id: ButtonId::GestureButton.into(),
            x: 8.,
            y: 380.,
            w: 44.,
            h: 80.,
        });
    }
    if controls.thumbwheel {
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
            !default_hotspots(FallbackControls::default())
                .iter()
                .any(|hotspot| { hotspot.id == MouseControlId::ThumbwheelRotation })
        );
        assert_eq!(
            default_hotspots(FallbackControls {
                thumbwheel: true,
                ..Default::default()
            })
            .iter()
            .filter(|hotspot| hotspot.id == MouseControlId::ThumbwheelRotation)
            .count(),
            1
        );
    }

    #[test]
    fn default_hotspots_expose_the_gesture_button() {
        let hotspots = default_hotspots(FallbackControls::default());
        assert!(
            hotspots
                .iter()
                .any(|h| { h.id == MouseControlId::Button(ButtonId::GestureButton) }),
            "the gesture button must be a mappable hotspot in the synthetic model"
        );
    }

    #[test]
    fn default_hotspots_omit_primary_clicks() {
        let hotspots = default_hotspots(FallbackControls::default());
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

    #[test]
    fn gaming_buttons_replace_dpi_toggle_and_gesture_button() {
        let hotspots = default_hotspots(FallbackControls {
            gaming_buttons: true,
            ..Default::default()
        });
        for stale in [ButtonId::DpiToggle, ButtonId::GestureButton] {
            assert!(
                !hotspots
                    .iter()
                    .any(|h| h.id == MouseControlId::Button(stale)),
                "{stale:?} does not exist on a gaming mouse and must not be an offered hotspot"
            );
        }
        for expected in [
            ButtonId::DpiShift,
            ButtonId::ProfileCycle,
            ButtonId::DpiUp,
            ButtonId::DpiDown,
        ] {
            assert!(
                hotspots
                    .iter()
                    .any(|h| h.id == MouseControlId::Button(expected)),
                "{expected:?} must be a mappable hotspot on a gaming mouse"
            );
        }
    }
}
