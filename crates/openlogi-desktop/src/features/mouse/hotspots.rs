//! Mouse hotspot geometry. Bounds are authored in model-local pixels (the
//! SVG canvas is 420×560 — see [`MOUSE_MODEL_SIZE`]) and
//! stored as plain `f32` tuples so this module stays purely data and doesn't
//! drag in `gpui` types.

use openlogi_core::binding::ButtonId;
use openlogi_core::device::{Capabilities, DeviceKind};

/// The measured capabilities that decide which targets the model draws.
///
/// Both bits come from the device's HID++ feature table. An unprobed device has
/// none, so [`Self::for_device`] falls back to the same presumption the tab gate
/// makes rather than to [`Default`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ModelControls {
    /// A horizontal thumb wheel is present — HID++ `0x2150` or a `0x6501`
    /// gesture descriptor.
    pub(crate) thumbwheel: bool,
    /// HID++ ReprogControls (`0x1b00`–`0x1b04`) are present, so controls beyond
    /// the OS-visible middle/back/forward can be diverted and remapped. Without
    /// it only the OS hook can remap this mouse, and only what the OS sees.
    pub(crate) can_divert: bool,
}

impl ModelControls {
    /// Derive the drawable control set from a device's measured capabilities.
    ///
    /// Devices that were offline at startup have none, so this presumes the
    /// same set the tab gate presumes — the two must agree, or a device is
    /// offered a panel whose contents were computed from different assumptions.
    pub(crate) fn for_device(capabilities: Option<Capabilities>, kind: DeviceKind) -> Self {
        let capabilities = capabilities.unwrap_or_else(|| Capabilities::presumed_from_kind(kind));
        Self {
            thumbwheel: capabilities.thumbwheel,
            can_divert: capabilities.can_divert_buttons(),
        }
    }
}

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

/// Drop the targets this device cannot actually remap.
///
/// A mouse with no ReprogControls (`0x1b04`) — every G-series mouse, which
/// carries `0x8100` OnboardProfiles instead — can only be remapped by the OS
/// input hook, and the hook sees exactly middle/back/forward. Offering a DPI
/// or gesture hotspot there would accept a binding that can never fire.
pub(crate) fn retain_remappable_hotspots(hotspots: &mut Vec<Hotspot>, can_divert: bool) {
    if can_divert {
        return;
    }
    hotspots.retain(|hotspot| hotspot.id.button().is_some_and(ButtonId::is_os_hook_button));
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

    /// A device that was offline at startup has no measured capabilities, so
    /// the model must presume the same set `DetailTab::tabs_for` presumes —
    /// otherwise a sleeping MX Master is handed the Buttons panel and then
    /// shown three hotspots out of six.
    #[test]
    fn unprobed_mouse_keeps_the_presumed_full_model() {
        let controls = ModelControls::for_device(None, DeviceKind::Mouse);
        assert!(
            controls.can_divert,
            "an unprobed mouse must not lose hotspots the tab gate assumes it has"
        );
    }

    /// A probed G-series mouse is a *measurement*, not a missing one, and the
    /// restriction it measured must survive into the model. Built from the real
    /// feature table so this tests the derivation rather than restating a
    /// hand-written struct literal back to itself.
    #[test]
    fn probed_gaming_mouse_gets_the_model_but_cannot_divert() {
        let g502 = Capabilities::from_feature_ids(&[0x8100, 0x8110, 0x2201, 0x2121]);
        let controls = ModelControls::for_device(Some(g502), DeviceKind::Mouse);
        assert!(g502.buttons, "the panel itself must still be offered");
        assert!(
            !controls.can_divert,
            "no 0x1b04 means the model may only draw what the OS hook sees"
        );
    }

    /// A G-series mouse (no `0x1b04`) keeps only what the OS hook can remap.
    /// The DPI toggle, the gesture button and the thumb wheel are HID++-only
    /// controls: without a capture feature they can never fire, so offering
    /// them would take a binding and silently drop it.
    #[test]
    fn hook_only_mouse_keeps_only_os_hook_targets() {
        let mut hotspots = default_hotspots(true);
        retain_remappable_hotspots(&mut hotspots, false);
        assert_eq!(
            hotspots.iter().map(|h| h.id).collect::<Vec<_>>(),
            vec![
                MouseControlId::Button(ButtonId::MiddleClick),
                MouseControlId::Button(ButtonId::Back),
                MouseControlId::Button(ButtonId::Forward),
            ]
        );
    }

    /// The filter is a capability gate, not a rewrite: a mouse that *does*
    /// expose ReprogControls keeps every target, in its authored order.
    #[test]
    fn divertable_mouse_keeps_every_target() {
        let mut hotspots = default_hotspots(true);
        let before = hotspots.clone();
        retain_remappable_hotspots(&mut hotspots, true);
        assert_eq!(hotspots, before);
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
