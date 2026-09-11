//! Mouse hotspot geometry. Bounds are authored in model-local pixels (the
//! SVG canvas is 420×560 — see [`MOUSE_MODEL_SIZE`]) and
//! stored as plain `f32` tuples so this module stays purely data and doesn't
//! drag in `gpui` types.

use openlogi_core::binding::{ButtonId, SpyModel};

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

/// MX-only controls that the G502 X Plus does not have. They occupy the same
/// silhouette slots we use for G6–G9, so they must come off before merge.
const G502_OMIT: [MouseControlId; 3] = [
    MouseControlId::Button(ButtonId::GestureButton),
    MouseControlId::Button(ButtonId::DpiToggle),
    MouseControlId::Button(ButtonId::HapticPanel),
];

/// Per-model silhouette overlay for a [`SpyModel`]. Gated on the HID++ key so
/// DPI Up/Down never appear on MX or an undocumented cousin.
pub struct SpyOverlay {
    /// MX slots this model's extras replace.
    pub omit: &'static [MouseControlId],
    /// Synthetic extra-button targets (G6–G9 on the G502 X Plus).
    pub hotspots: [Hotspot; 4],
}

/// Overlay for this HID++ model id, if it has a locked spy map.
#[must_use]
pub fn spy_overlay_for(config_key: &str) -> Option<SpyOverlay> {
    SpyModel::for_hidpp_key(config_key)?;
    Some(SpyOverlay {
        omit: &G502_OMIT,
        hotspots: spy_hotspots(),
    })
}

/// Synthetic G6–G9 targets for the G502 X Plus silhouette. Coordinates reuse
/// the MX left-thumb and DPI-cluster slots so the extras are visible; this
/// mouse has no depot `slotId`s. Other models add their own overlay in
/// [`spy_overlay_for`] — do not reuse these numbers.
#[must_use]
pub fn spy_hotspots() -> [Hotspot; 4] {
    [
        // Left-thumb sniper — the MX silhouette's gesture-button slot.
        Hotspot {
            id: ButtonId::DpiShift.into(),
            x: 8.,
            y: 380.,
            w: 44.,
            h: 80.,
        },
        // DPI cluster, left of the old ModeShift pad.
        Hotspot {
            id: ButtonId::DpiDown.into(),
            x: 130.,
            y: 226.,
            w: 48.,
            h: 44.,
        },
        // DPI cluster, right of the old ModeShift pad.
        Hotspot {
            id: ButtonId::DpiUp.into(),
            x: 242.,
            y: 226.,
            w: 48.,
            h: 44.,
        },
        // Profile cycle — immediately behind the DPI cluster.
        Hotspot {
            id: ButtonId::ProfileCycle.into(),
            x: 175.,
            y: 278.,
            w: 70.,
            h: 40.,
        },
    ]
}

/// Append this model's extra-button targets when `config_key` is a locked
/// spy model. MX (`2b042`) and undocumented cousins get no DPI Up/Down.
pub fn merge_spy_hotspots(
    hotspots: &mut Vec<Hotspot>,
    config_key: Option<&str>,
    scale: (f32, f32),
) {
    let Some(overlay) = config_key.and_then(spy_overlay_for) else {
        return;
    };
    hotspots.retain(|hotspot| !overlay.omit.contains(&hotspot.id));
    for hotspot in overlay.hotspots {
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
        assert!(spy_overlay_for("04099").is_some());
        assert!(spy_overlay_for("2b042").is_none());
        assert!(
            spy_overlay_for("0409f").is_none(),
            "a cousin without a watch dump must not inherit the Plus overlay"
        );
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
        assert!(
            !g502.iter().any(|h| G502_OMIT.contains(&h.id)),
            "MX-only slots must yield to G6–G9 on the G502 silhouette"
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
