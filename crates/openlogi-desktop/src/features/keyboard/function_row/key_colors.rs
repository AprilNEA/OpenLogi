//! The colours of a keypad's LCD key: one shared palette for its background,
//! text, and icon, a custom hex field beside each, and the background's
//! gradient mode with its presets — plus the small pure rules behind them.

use crate::ui::theme::FocusRingStyle as _;
use gpui::{
    App, Background, Entity, InteractiveElement, IntoElement, ParentElement, Role, SharedString,
    StatefulInteractiveElement as _, Styled, Toggled, div, linear_color_stop, linear_gradient,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::button::{Button, ButtonGroup};
use gpui_kit::component::input::InputState;
use gpui_kit::component::{Selectable as _, Sizable as _, h_flex, v_flex};
use openlogi_core::binding::GridSlot;
use openlogi_core::color::Rgb;
use openlogi_core::config::{GradientDirection, KeyFaceStyle, KeyGradient};

use super::FunctionRowView;
use super::key_face::commit_face;
use crate::features::binding_editor::editor_section;
use crate::ui::components::control_input;
use crate::ui::theme::{self, Palette, SelectableStyle as _, Typography as _};

const SWATCH: f32 = 18.;

/// The LCD's own colours when a face sets none: white on black.
pub(super) const DEFAULT_BACKGROUND: Rgb = Rgb::new(0, 0, 0);
pub(super) const DEFAULT_COLOR: Rgb = Rgb::WHITE;

/// The colours every chooser offers: neutrals from black to white, the
/// saturated hues around the wheel, deep muted tones that keep light text
/// readable, and a few light tints that carry dark text.
pub(super) const PALETTE: [Rgb; 30] = [
    // Neutrals.
    Rgb::new(0x00, 0x00, 0x00),
    Rgb::new(0x1c, 0x1c, 0x1e),
    Rgb::new(0x3a, 0x3a, 0x3c),
    Rgb::new(0x63, 0x63, 0x66),
    Rgb::new(0x8e, 0x8e, 0x93),
    Rgb::new(0xc7, 0xc7, 0xcc),
    Rgb::new(0xff, 0xff, 0xff),
    // Saturated hues.
    Rgb::new(0xff, 0x3b, 0x30),
    Rgb::new(0xff, 0x95, 0x00),
    Rgb::new(0xff, 0xcc, 0x00),
    Rgb::new(0x34, 0xc7, 0x59),
    Rgb::new(0x00, 0xc7, 0xbe),
    Rgb::new(0x30, 0xb0, 0xc7),
    Rgb::new(0x32, 0xad, 0xe6),
    Rgb::new(0x00, 0x7a, 0xff),
    Rgb::new(0x58, 0x56, 0xd6),
    Rgb::new(0xaf, 0x52, 0xde),
    Rgb::new(0xff, 0x2d, 0x55),
    Rgb::new(0xa2, 0x84, 0x5e),
    // Deep, muted tones.
    Rgb::new(0x7a, 0x1f, 0x1f),
    Rgb::new(0x7a, 0x4a, 0x00),
    Rgb::new(0x6b, 0x5b, 0x00),
    Rgb::new(0x1f, 0x5f, 0x2e),
    Rgb::new(0x0f, 0x4c, 0x5c),
    Rgb::new(0x1e, 0x3a, 0x8a),
    Rgb::new(0x4c, 0x1d, 0x95),
    Rgb::new(0x83, 0x18, 0x43),
    // Light tints.
    Rgb::new(0xff, 0xd6, 0xa5),
    Rgb::new(0xca, 0xff, 0xbf),
    Rgb::new(0xa0, 0xc4, 0xff),
];

/// Gradient presets, as `(from, to)`: sunset, ocean, forest, fire, night,
/// candy, steel, mint, gold, berry, sky, mono.
pub(super) const GRADIENT_PRESETS: [(Rgb, Rgb); 12] = [
    (Rgb::new(0xff, 0x5a, 0x00), Rgb::new(0x78, 0x00, 0xc8)),
    (Rgb::new(0x00, 0x57, 0xff), Rgb::new(0x00, 0xc7, 0xbe)),
    (Rgb::new(0x1f, 0x7a, 0x1f), Rgb::new(0xa3, 0xe6, 0x35)),
    (Rgb::new(0xff, 0x1e, 0x1e), Rgb::new(0xff, 0x95, 0x00)),
    (Rgb::new(0x1e, 0x3a, 0x8a), Rgb::new(0x00, 0x00, 0x00)),
    (Rgb::new(0xff, 0x4f, 0xa3), Rgb::new(0x8b, 0x5c, 0xf6)),
    (Rgb::new(0x8e, 0x8e, 0x93), Rgb::new(0x00, 0x00, 0x00)),
    (Rgb::new(0x34, 0xd3, 0x99), Rgb::new(0x22, 0xd3, 0xee)),
    (Rgb::new(0xff, 0xd6, 0x0a), Rgb::new(0xff, 0x9f, 0x0a)),
    (Rgb::new(0x7e, 0x22, 0xce), Rgb::new(0xec, 0x48, 0x99)),
    (Rgb::new(0x7d, 0xd3, 0xfc), Rgb::new(0x25, 0x63, 0xeb)),
    (Rgb::new(0xff, 0xff, 0xff), Rgb::new(0x8e, 0x8e, 0x93)),
];

/// One colour of a face a chooser edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ColorField {
    Background,
    Text,
    Icon,
    GradientFrom,
    GradientTo,
}

impl ColorField {
    /// Every field, in the order the face draft keeps their hex inputs.
    pub(super) const ALL: [Self; 5] = [
        Self::Background,
        Self::Text,
        Self::Icon,
        Self::GradientFrom,
        Self::GradientTo,
    ];

    /// The field's place in [`Self::ALL`].
    pub(super) fn index(self) -> usize {
        match self {
            Self::Background => 0,
            Self::Text => 1,
            Self::Icon => 2,
            Self::GradientFrom => 3,
            Self::GradientTo => 4,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Background => "key-face-background",
            Self::Text => "key-face-color",
            Self::Icon => "key-face-icon-color",
            Self::GradientFrom => "key-face-gradient-from",
            Self::GradientTo => "key-face-gradient-to",
        }
    }

    /// The colour `style` sets for this field, `None` while it follows its
    /// default.
    pub(super) fn get(self, style: &KeyFaceStyle) -> Option<Rgb> {
        match self {
            Self::Background => style.background,
            Self::Text => style.color,
            Self::Icon => style.icon_color,
            Self::GradientFrom => style.gradient.map(|gradient| gradient.from),
            Self::GradientTo => style.gradient.map(|gradient| gradient.to),
        }
    }

    /// Set this field of `style` to `color`, `None` restoring its default.
    /// A gradient end is always a colour: setting one on a face without a
    /// gradient starts from the first preset, and `None` leaves it as it is.
    pub(super) fn set(self, style: &mut KeyFaceStyle, color: Option<Rgb>) {
        match self {
            Self::Background => style.background = color,
            Self::Text => style.color = color,
            Self::Icon => style.icon_color = color,
            Self::GradientFrom | Self::GradientTo => {
                let Some(color) = color else { return };
                let mut gradient = style.gradient.unwrap_or_else(|| first_preset(None));
                if self == Self::GradientFrom {
                    gradient.from = color;
                } else {
                    gradient.to = color;
                }
                style.gradient = Some(gradient);
            }
        }
    }

    /// What the field's default swatch shows, if it has one.
    fn default_swatch(self) -> Option<(Rgb, SharedString)> {
        match self {
            Self::Background => Some((DEFAULT_BACKGROUND, tr!("keyboard.key_color_default"))),
            Self::Text => Some((DEFAULT_COLOR, tr!("keyboard.key_color_default"))),
            Self::Icon => Some((DEFAULT_COLOR, tr!("keyboard.key_icon_color_same_as_text"))),
            Self::GradientFrom | Self::GradientTo => None,
        }
    }
}

/// The colour typed into a custom field: six hex digits, with or without a
/// leading `#`, surrounding space ignored.
pub(super) fn parse_hex(text: &str) -> Option<Rgb> {
    let text = text.trim();
    text.strip_prefix('#').unwrap_or(text).parse().ok()
}

/// The first gradient preset, running `direction` (top to bottom when
/// unset).
fn first_preset(direction: Option<GradientDirection>) -> KeyGradient {
    let (from, to) = GRADIENT_PRESETS[0];
    KeyGradient {
        from,
        to,
        direction: direction.unwrap_or_default(),
    }
}

/// Put preset `(from, to)` on `style`'s background, keeping the direction a
/// gradient already runs.
pub(super) fn apply_gradient_preset(style: &mut KeyFaceStyle, (from, to): (Rgb, Rgb)) {
    let direction = style.gradient.map(|gradient| gradient.direction);
    style.gradient = Some(KeyGradient {
        from,
        to,
        direction: direction.unwrap_or_default(),
    });
}

/// How the background section draws: one colour, or a gradient.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BackgroundMode {
    Solid,
    Gradient,
}

impl BackgroundMode {
    pub(super) fn of(style: &KeyFaceStyle) -> Self {
        if style.gradient.is_some() {
            Self::Gradient
        } else {
            Self::Solid
        }
    }

    /// Switch `style` to this mode: a solid background drops the gradient,
    /// and a gradient — when the face has none yet — starts from the first
    /// preset.
    pub(super) fn apply(self, style: &mut KeyFaceStyle) {
        match self {
            Self::Solid => style.gradient = None,
            Self::Gradient => {
                if style.gradient.is_none() {
                    style.gradient = Some(first_preset(None));
                }
            }
        }
    }
}

/// The colour the face draws its action icon or Lucide symbol in: its own
/// icon colour, else the text colour, else white.
pub(super) fn icon_color(style: &KeyFaceStyle) -> Rgb {
    style.icon_color.or(style.color).unwrap_or(DEFAULT_COLOR)
}

/// The gradient's angle for GPUI's `linear_gradient`, which measures from
/// the top, clockwise.
pub(super) fn gradient_angle(direction: GradientDirection) -> f32 {
    match direction {
        GradientDirection::Vertical => 180.,
        GradientDirection::Horizontal => 90.,
        GradientDirection::Diagonal => 135.,
    }
}

/// `gradient` as a GPUI fill.
pub(super) fn gradient_fill(gradient: KeyGradient) -> Background {
    linear_gradient(
        gradient_angle(gradient.direction),
        linear_color_stop(rgb(gradient.from.packed()), 0.),
        linear_color_stop(rgb(gradient.to.packed()), 1.),
    )
}

impl FunctionRowView {
    /// Save the colour typed into `field`'s custom input, or flag the input
    /// when it holds no colour. A blank input saves nothing.
    pub(super) fn commit_face_hex(&mut self, field: ColorField, cx: &mut App) {
        let Some(draft) = self.appearance.draft() else {
            return;
        };
        let slot = draft.slot;
        let text = draft.hex[field.index()].read(cx).value().to_string();
        if text.trim().is_empty() {
            self.appearance.set_hex_invalid(None);
            return;
        }
        match parse_hex(&text) {
            Some(color) => {
                self.appearance.set_hex_invalid(None);
                commit_face(cx, slot, |style| field.set(style, Some(color)));
            }
            None => self.appearance.set_hex_invalid(Some(field)),
        }
    }
}

/// The background section: the solid/gradient switch, then the solid
/// colour chooser or the gradient's presets, ends and direction.
pub(super) fn background_section(
    slot: GridSlot,
    style: &KeyFaceStyle,
    hex: &[Entity<InputState>; 5],
    invalid: Option<ColorField>,
    pal: Palette,
    cx: &App,
) -> gpui::Div {
    let mode = BackgroundMode::of(style);
    let modes = [BackgroundMode::Solid, BackgroundMode::Gradient];
    let switch = ButtonGroup::new("key-face-background-mode")
        .outline()
        .small()
        .children(modes.iter().map(|candidate| {
            let (id, label) = match candidate {
                BackgroundMode::Solid => ("key-face-solid", tr!("keyboard.key_background_solid")),
                BackgroundMode::Gradient => {
                    ("key-face-gradient", tr!("keyboard.key_background_gradient"))
                }
            };
            Button::new(id)
                .label(label)
                .selected(*candidate == mode)
                .selected_segment(*candidate == mode, cx)
        }))
        .on_click(move |indices: &Vec<usize>, _, cx| {
            if let Some(mode) = indices.first().and_then(|index| modes.get(*index)) {
                commit_face(cx, slot, |style| mode.apply(style));
            }
        });
    let body = match (mode, style.gradient) {
        (BackgroundMode::Gradient, Some(gradient)) => {
            gradient_body(slot, gradient, hex, invalid, pal, cx).into_any_element()
        }
        _ => {
            color_chooser(ColorField::Background, slot, style, hex, invalid, pal).into_any_element()
        }
    };
    v_flex()
        .gap_1p5()
        .child(div().px_2().child(switch))
        .child(body)
}

/// The gradient's presets, its two ends, and its direction.
fn gradient_body(
    slot: GridSlot,
    gradient: KeyGradient,
    hex: &[Entity<InputState>; 5],
    invalid: Option<ColorField>,
    pal: Palette,
    cx: &App,
) -> gpui::Div {
    let style = KeyFaceStyle {
        gradient: Some(gradient),
        ..KeyFaceStyle::default()
    };
    let presets = h_flex()
        .px_2()
        .gap_1p5()
        .flex_wrap()
        .debug_selector(|| "key-face-gradient-presets".into())
        .children(GRADIENT_PRESETS.iter().enumerate().map(|(index, preset)| {
            let (from, to) = *preset;
            let selected = gradient.from == from && gradient.to == to;
            let fill = gradient_fill(KeyGradient {
                from,
                to,
                direction: gradient.direction,
            });
            swatch_button(
                SharedString::from(format!("key-face-gradient-preset-{index}")),
                selected,
                format!("#{:06X} → #{:06X}", from.packed(), to.packed()).into(),
                pal,
            )
            .size(px(SWATCH + 6.))
            .bg(fill)
            .on_click(move |_, _, cx| {
                commit_face(cx, slot, |style| apply_gradient_preset(style, (from, to)));
            })
        }));
    let directions = GradientDirection::ALL;
    let direction = ButtonGroup::new("key-face-gradient-direction")
        .outline()
        .small()
        .children(directions.iter().map(|candidate| {
            let (id, label) = match candidate {
                GradientDirection::Vertical => (
                    "key-face-gradient-vertical",
                    tr!("keyboard.key_gradient_vertical"),
                ),
                GradientDirection::Horizontal => (
                    "key-face-gradient-horizontal",
                    tr!("keyboard.key_gradient_horizontal"),
                ),
                GradientDirection::Diagonal => (
                    "key-face-gradient-diagonal",
                    tr!("keyboard.key_gradient_diagonal"),
                ),
            };
            Button::new(id)
                .label(label)
                .selected(*candidate == gradient.direction)
                .selected_segment(*candidate == gradient.direction, cx)
        }))
        .on_click(move |indices: &Vec<usize>, _, cx| {
            if let Some(direction) = indices.first().and_then(|index| directions.get(*index)) {
                let direction = *direction;
                commit_face(cx, slot, |style| {
                    if let Some(gradient) = style.gradient.as_mut() {
                        gradient.direction = direction;
                    }
                });
            }
        });
    let end = |field: ColorField, label: SharedString| {
        v_flex()
            .gap_1()
            .child(
                div()
                    .px_2()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(label),
            )
            .child(color_chooser(field, slot, &style, hex, invalid, pal))
    };
    v_flex()
        .gap_1p5()
        .child(presets)
        .child(div().px_2().child(direction))
        .child(end(
            ColorField::GradientFrom,
            tr!("keyboard.key_gradient_from"),
        ))
        .child(end(ColorField::GradientTo, tr!("keyboard.key_gradient_to")))
}

/// The text colour and icon colour choosers, each under its heading.
pub(super) fn text_and_icon_colors(
    slot: GridSlot,
    style: &KeyFaceStyle,
    hex: &[Entity<InputState>; 5],
    invalid: Option<ColorField>,
    pal: Palette,
) -> gpui::Div {
    v_flex()
        .gap_1()
        .child(editor_section(tr!("keyboard.key_text_color"), pal))
        .child(color_chooser(
            ColorField::Text,
            slot,
            style,
            hex,
            invalid,
            pal,
        ))
        .child(editor_section(tr!("keyboard.key_icon_color"), pal))
        .child(color_chooser(
            ColorField::Icon,
            slot,
            style,
            hex,
            invalid,
            pal,
        ))
}

/// One colour chooser: the field's default swatch (where it has one), the
/// shared palette, and the custom hex input with its error.
fn color_chooser(
    field: ColorField,
    slot: GridSlot,
    style: &KeyFaceStyle,
    hex: &[Entity<InputState>; 5],
    invalid: Option<ColorField>,
    pal: Palette,
) -> gpui::Div {
    let current = field.get(style);
    let id = field.id();
    let default = field.default_swatch().map(|(color, label)| {
        swatch_button(
            SharedString::from(format!("{id}-default")),
            current.is_none(),
            label,
            pal,
        )
        .bg(rgb(color.packed()))
        .on_click(move |_, _, cx| commit_face(cx, slot, |style| field.set(style, None)))
    });
    let swatches = PALETTE.iter().map(move |color| {
        let color = *color;
        swatch_button(
            SharedString::from(format!("{id}-{:06x}", color.packed())),
            current == Some(color),
            format!("#{:06X}", color.packed()).into(),
            pal,
        )
        .bg(rgb(color.packed()))
        .on_click(move |_, _, cx| commit_face(cx, slot, |style| field.set(style, Some(color))))
    });
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .px_2()
                .gap_1()
                .flex_wrap()
                .debug_selector(move || format!("{id}-swatches"))
                .children(default)
                .children(swatches),
        )
        .child(
            div()
                .px_2()
                .debug_selector(move || format!("{id}-hex"))
                .child(control_input(&hex[field.index()])),
        )
        .when(invalid == Some(field), |chooser| {
            chooser.child(
                div()
                    .debug_selector(|| "key-face-color-invalid".into())
                    .px_2()
                    .text_caption()
                    .text_color(pal.text_danger)
                    .child(tr!("keyboard.key_color_invalid")),
            )
        })
}

/// A square swatch button, outlined in the accent while selected; the
/// caller paints its fill.
fn swatch_button(
    id: SharedString,
    selected: bool,
    label: SharedString,
    pal: Palette,
) -> BaseButton {
    BaseButton::new(id)
        .role(Role::RadioButton)
        .selected(selected)
        .accessibility_label(label.clone())
        .aria_toggled(if selected {
            Toggled::True
        } else {
            Toggled::False
        })
        .aria_selected(selected)
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(label.clone()).build(window, cx)
        })
        .size(px(SWATCH))
        .rounded(pal.control_radius)
        .border_2()
        .border_color(if selected {
            theme::accent()
        } else {
            pal.border
        })
        .cursor_pointer()
        .focus_visible(move |style| style.focus_ring(pal))
}
