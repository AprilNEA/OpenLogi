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
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Button(button) => button.label(),
            Self::ThumbwheelRotation => "Thumb Wheel",
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
///
/// `standard_buttons_only` drops `DpiToggle`/`GestureButton`: those need a
/// diverted HID++ control (`0x1b04`) to ever reach the OS hook, which
/// `OnboardProfiles`-only devices (G-series gaming mice — no `ReprogControls`
/// at all) don't have. Offering those hotspots there would show a control
/// that can never actually fire. `MiddleClick`/`Back`/`Forward` stay: on
/// every mouse the OS hook captures those natively, with no HID++ diversion
/// involved (see `crates/openlogi-hook`), so they work identically either way.
#[must_use]
pub fn default_hotspots(thumbwheel: bool, standard_buttons_only: bool) -> Vec<Hotspot> {
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
    if !standard_buttons_only {
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
            !default_hotspots(false, false)
                .iter()
                .any(|hotspot| { hotspot.id == MouseControlId::ThumbwheelRotation })
        );
        assert_eq!(
            default_hotspots(true, false)
                .iter()
                .filter(|hotspot| hotspot.id == MouseControlId::ThumbwheelRotation)
                .count(),
            1
        );
    }

    #[test]
    fn default_hotspots_expose_the_gesture_button() {
        let hotspots = default_hotspots(false, false);
        assert!(
            hotspots
                .iter()
                .any(|h| { h.id == MouseControlId::Button(ButtonId::GestureButton) }),
            "the gesture button must be a mappable hotspot in the synthetic model"
        );
    }

    #[test]
    fn default_hotspots_omit_primary_clicks() {
        let hotspots = default_hotspots(false, false);
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

    /// G-series gaming mice (`OnboardProfiles`, no `ReprogControls`) have no
    /// diverted DPI-toggle/gesture-button signal, so those hotspots must be
    /// absent — offering a hotspot that can never fire would be misleading.
    #[test]
    fn standard_buttons_only_drops_dpi_toggle_and_gesture_button() {
        let hotspots = default_hotspots(false, true);
        assert!(
            !hotspots
                .iter()
                .any(|h| h.id == MouseControlId::Button(ButtonId::DpiToggle)),
            "DPI toggle has no native signal on an OnboardProfiles-only mouse"
        );
        assert!(
            !hotspots
                .iter()
                .any(|h| h.id == MouseControlId::Button(ButtonId::GestureButton)),
            "the gesture button has no native signal on an OnboardProfiles-only mouse"
        );
    }

    /// Middle/Back/Forward are native OS-hook buttons on any mouse — they
    /// must stay available even in the reduced `standard_buttons_only` set.
    #[test]
    fn standard_buttons_only_keeps_middle_back_forward() {
        let hotspots = default_hotspots(false, true);
        for expected in [ButtonId::MiddleClick, ButtonId::Back, ButtonId::Forward] {
            assert!(
                hotspots
                    .iter()
                    .any(|h| h.id == MouseControlId::Button(expected)),
                "{expected:?} must remain a hotspot in the reduced set"
            );
        }
    }
}
