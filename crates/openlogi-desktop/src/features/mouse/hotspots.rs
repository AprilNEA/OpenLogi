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

/// Synthetic G6–G9 targets for the G502 X Plus silhouette. Coordinates are
/// approximate model-local pixels — this mouse has no depot `slotId`s.
#[must_use]
pub fn spy_hotspots() -> [Hotspot; 4] {
    [
        Hotspot {
            id: ButtonId::DpiShift.into(),
            x: 0.,
            y: 360.,
            w: 40.,
            h: 50.,
        },
        Hotspot {
            id: ButtonId::DpiDown.into(),
            x: 130.,
            y: 250.,
            w: 40.,
            h: 36.,
        },
        Hotspot {
            id: ButtonId::DpiUp.into(),
            x: 250.,
            y: 250.,
            w: 40.,
            h: 36.,
        },
        Hotspot {
            id: ButtonId::ProfileCycle.into(),
            x: 175.,
            y: 300.,
            w: 70.,
            h: 36.,
        },
    ]
}

/// Append G6–G9 targets when `config_key` is the G502 X Plus, even if a depot
/// PNG already contributed other hotspots.
pub fn merge_spy_hotspots(
    hotspots: &mut Vec<Hotspot>,
    config_key: Option<&str>,
    scale: (f32, f32),
) {
    if config_key.is_none_or(|key| ButtonId::spy_buttons_for_config_key(key).is_none()) {
        return;
    }
    for hotspot in spy_hotspots() {
        if hotspots.iter().any(|existing| existing.id == hotspot.id) {
            continue;
        }
        hotspots.push(Hotspot {
            x: hotspot.x * scale.0,
            y: hotspot.y * scale.1,
            w: hotspot.w * scale.0,
            h: hotspot.h * scale.1,
            id: hotspot.id,
        });
    }
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
    fn spy_hotspots_cover_g6_through_g9() {
        let ids: Vec<_> = spy_hotspots().into_iter().map(|h| h.id).collect();
        for button in ButtonId::SPY_BUTTONS {
            assert!(
                ids.contains(&MouseControlId::Button(button)),
                "{button:?} must have a synthetic hotspot"
            );
        }
    }

    #[test]
    fn merge_spy_hotspots_is_gated_on_the_g502() {
        let mut mx = default_hotspots(false);
        merge_spy_hotspots(&mut mx, Some("2b042"), (1., 1.));
        assert!(!mx.iter().any(|h| h.id == ButtonId::DpiUp.into()));

        let mut g502 = default_hotspots(false);
        merge_spy_hotspots(&mut g502, Some("04099"), (1., 1.));
        assert_eq!(
            g502.iter()
                .filter(|h| matches!(
                    h.id,
                    MouseControlId::Button(
                        ButtonId::DpiShift
                            | ButtonId::DpiUp
                            | ButtonId::DpiDown
                            | ButtonId::ProfileCycle
                    )
                ))
                .count(),
            4
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
