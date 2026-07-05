//! Device-family-specific button layouts for the mouse model.
//!
//! The G502 family does not expose the MX-line `0x1b04` reprogrammable-control
//! table, so this layout is model-authored rather than discovered from firmware.

use openlogi_core::device::{DeviceModelInfo, is_g502_family};

use crate::data::mouse_buttons::{ButtonId, Hotspot, default_hotspots};
use crate::mouse_model::leader_lines::{Label, Side};

/// G Hub splits the G502 button map into top and thumb-side perspectives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseModelPerspective {
    #[default]
    View1,
    View2,
}

impl MouseModelPerspective {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::View1 => "View 1",
            Self::View2 => "View 2",
        }
    }
}

/// Which fallback/hotspot policy the mouse model should use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseButtonLayout {
    #[default]
    Default,
    G502,
}

impl MouseButtonLayout {
    #[must_use]
    pub fn for_device(model: Option<&DeviceModelInfo>, name: Option<&str>) -> Self {
        if is_g502_family(model, name) {
            Self::G502
        } else {
            Self::Default
        }
    }

    #[must_use]
    pub fn keeps_hotspot(self, id: ButtonId) -> bool {
        match self {
            Self::Default => true,
            Self::G502 => matches!(
                id,
                ButtonId::MiddleClick
                    | ButtonId::Back
                    | ButtonId::Forward
                    | ButtonId::DpiUp
                    | ButtonId::DpiDown
                    | ButtonId::DpiShift
                    | ButtonId::WheelLeft
                    | ButtonId::WheelRight
                    | ButtonId::SmartShift
            ),
        }
    }

    #[must_use]
    pub fn binds_hotspot(self, id: ButtonId) -> bool {
        match self {
            Self::Default => true,
            // G502 extras need the gaming-button protocol before a saved binding
            // can actually fire; keep them visible but not editable for now.
            Self::G502 => matches!(
                id,
                ButtonId::MiddleClick | ButtonId::Back | ButtonId::Forward
            ),
        }
    }

    #[must_use]
    pub fn supports_perspectives(self) -> bool {
        matches!(self, Self::G502)
    }

    #[must_use]
    pub fn fallback_hotspots(self, perspective: MouseModelPerspective) -> Vec<Hotspot> {
        match self {
            Self::Default => default_hotspots(),
            Self::G502 => g502_hotspots(perspective),
        }
    }

    #[must_use]
    pub fn fallback_labels(self, perspective: MouseModelPerspective) -> Vec<Label> {
        match self {
            Self::Default => default_labels(),
            Self::G502 => g502_labels(perspective),
        }
    }
}

fn default_labels() -> Vec<Label> {
    vec![
        Label {
            id: ButtonId::MiddleClick,
            side: Side::Left,
            y: 120.,
        },
        Label {
            id: ButtonId::Back,
            side: Side::Left,
            y: 240.,
        },
        Label {
            id: ButtonId::Forward,
            side: Side::Left,
            y: 340.,
        },
        Label {
            id: ButtonId::DpiToggle,
            side: Side::Left,
            y: 430.,
        },
        Label {
            id: ButtonId::GestureButton,
            side: Side::Left,
            y: 510.,
        },
    ]
}

fn g502_hotspots(perspective: MouseModelPerspective) -> Vec<Hotspot> {
    match perspective {
        MouseModelPerspective::View1 => g502_top_hotspots(),
        MouseModelPerspective::View2 => g502_side_hotspots(),
    }
}

fn g502_top_hotspots() -> Vec<Hotspot> {
    vec![
        Hotspot {
            id: ButtonId::SmartShift,
            x: 168.,
            y: 302.,
            w: 84.,
            h: 44.,
        },
        Hotspot {
            id: ButtonId::WheelLeft,
            x: 140.,
            y: 122.,
            w: 42.,
            h: 54.,
        },
        Hotspot {
            id: ButtonId::MiddleClick,
            x: 180.,
            y: 110.,
            w: 60.,
            h: 90.,
        },
        Hotspot {
            id: ButtonId::WheelRight,
            x: 238.,
            y: 122.,
            w: 42.,
            h: 54.,
        },
        Hotspot {
            id: ButtonId::DpiUp,
            x: 116.,
            y: 184.,
            w: 48.,
            h: 48.,
        },
        Hotspot {
            id: ButtonId::DpiDown,
            x: 94.,
            y: 242.,
            w: 48.,
            h: 48.,
        },
    ]
}

fn g502_side_hotspots() -> Vec<Hotspot> {
    vec![
        Hotspot {
            id: ButtonId::DpiShift,
            x: 160.,
            y: 126.,
            w: 58.,
            h: 64.,
        },
        Hotspot {
            id: ButtonId::Forward,
            x: 186.,
            y: 198.,
            w: 58.,
            h: 64.,
        },
        Hotspot {
            id: ButtonId::Back,
            x: 202.,
            y: 270.,
            w: 58.,
            h: 64.,
        },
    ]
}

fn g502_labels(perspective: MouseModelPerspective) -> Vec<Label> {
    match perspective {
        MouseModelPerspective::View1 => g502_top_labels(),
        MouseModelPerspective::View2 => g502_side_labels(),
    }
}

fn g502_top_labels() -> Vec<Label> {
    vec![
        Label {
            id: ButtonId::MiddleClick,
            side: Side::Left,
            y: 80.,
        },
        Label {
            id: ButtonId::WheelLeft,
            side: Side::Left,
            y: 160.,
        },
        Label {
            id: ButtonId::WheelRight,
            side: Side::Left,
            y: 240.,
        },
        Label {
            id: ButtonId::SmartShift,
            side: Side::Left,
            y: 320.,
        },
        Label {
            id: ButtonId::DpiUp,
            side: Side::Left,
            y: 400.,
        },
        Label {
            id: ButtonId::DpiDown,
            side: Side::Left,
            y: 480.,
        },
    ]
}

fn g502_side_labels() -> Vec<Label> {
    vec![
        Label {
            id: ButtonId::DpiShift,
            side: Side::Left,
            y: 160.,
        },
        Label {
            id: ButtonId::Forward,
            side: Side::Left,
            y: 280.,
        },
        Label {
            id: ButtonId::Back,
            side: Side::Left,
            y: 400.,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::device::DeviceTransports;

    use crate::data::mouse_buttons::MOUSE_MODEL_SIZE;

    #[test]
    fn default_layout_includes_mx_gesture_button() {
        let labels = MouseButtonLayout::Default.fallback_labels(MouseModelPerspective::View1);

        assert!(
            labels.iter().any(|l| l.id == ButtonId::GestureButton),
            "the default MX-style fallback needs a gesture-button label"
        );
    }

    #[test]
    fn g502_layout_omits_mx_only_gesture_controls() {
        let hotspots = MouseButtonLayout::G502.fallback_hotspots(MouseModelPerspective::View1);

        assert!(
            hotspots.iter().any(|h| h.id == ButtonId::SmartShift),
            "G502 top controls should render"
        );
        assert!(!hotspots.iter().any(|h| h.id == ButtonId::DpiShift));
        assert!(
            !hotspots.iter().any(|h| h.id == ButtonId::GestureButton),
            "G502 should not render the MX gesture button"
        );
    }

    #[test]
    fn g502_layout_matches_live_model_identity() {
        let model = DeviceModelInfo {
            entity_count: 1,
            serial_number: None,
            unit_id: [0; 4],
            transports: DeviceTransports::default(),
            model_ids: [0x4099, 0xc095, 0],
            extended_model_id: 0,
        };

        assert_eq!(
            MouseButtonLayout::for_device(Some(&model), Some("G502 X PLUS")),
            MouseButtonLayout::G502
        );
    }

    #[test]
    fn fallback_hotspots_stay_inside_model_canvas() {
        for perspective in [MouseModelPerspective::View1, MouseModelPerspective::View2] {
            for hotspot in MouseButtonLayout::G502.fallback_hotspots(perspective) {
                assert!(hotspot.x >= 0.);
                assert!(hotspot.y >= 0.);
                assert!(hotspot.x + hotspot.w <= MOUSE_MODEL_SIZE.0);
                assert!(hotspot.y + hotspot.h <= MOUSE_MODEL_SIZE.1);
            }
        }
    }

    #[test]
    fn g502_side_view_contains_thumb_buttons() {
        let hotspots = MouseButtonLayout::G502.fallback_hotspots(MouseModelPerspective::View2);

        assert!(hotspots.iter().any(|h| h.id == ButtonId::DpiShift));
        assert!(hotspots.iter().any(|h| h.id == ButtonId::Forward));
        assert!(hotspots.iter().any(|h| h.id == ButtonId::Back));
        assert!(!hotspots.iter().any(|h| h.id == ButtonId::MiddleClick));
    }

    #[test]
    fn g502_native_extras_are_visible_but_not_bindable() {
        assert!(MouseButtonLayout::G502.keeps_hotspot(ButtonId::DpiUp));
        assert!(MouseButtonLayout::G502.keeps_hotspot(ButtonId::SmartShift));
        assert!(!MouseButtonLayout::G502.binds_hotspot(ButtonId::DpiUp));
        assert!(!MouseButtonLayout::G502.binds_hotspot(ButtonId::SmartShift));
        assert!(MouseButtonLayout::G502.binds_hotspot(ButtonId::MiddleClick));
        assert!(MouseButtonLayout::G502.binds_hotspot(ButtonId::Back));
        assert!(MouseButtonLayout::G502.binds_hotspot(ButtonId::Forward));
    }
}
