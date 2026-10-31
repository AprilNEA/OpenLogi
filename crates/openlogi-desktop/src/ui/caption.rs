//! The window controls of a window without a native caption
//! ([`crate::windows::CAPTION_IN_HEADER`]).
//!
//! The platform's own glyphs, because every other window on the desktop has the
//! same three, on chips of the height and radius of the actions beside them.
//! Clicks need no handlers: the control areas are hit-tested by the OS.

use gpui::{
    Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Rgba, Styled as _,
    Window, WindowControlArea, div, px, rgb,
};
use gpui_component::h_flex;

use crate::ui::theme::{CONTROL_H, HEADER_H, Palette, Typography as _};

/// Ships with every supported Windows, unlike the newer Segoe Fluent Icons.
const GLYPH_FONT: &str = "Segoe MDL2 Assets";
const GLYPH_MINIMIZE: &str = "\u{E921}";
const GLYPH_MAXIMIZE: &str = "\u{E922}";
const GLYPH_RESTORE: &str = "\u{E923}";
const GLYPH_CLOSE: &str = "\u{E8BB}";

/// Each hit area spans the bar, and the last runs on into the window corner, so
/// the pointer can be thrown there; only the chip inside it shows.
const CELL_W: Pixels = px(40.);
const CORNER_CELL_W: Pixels = px(52.);
const CHIP_W: Pixels = px(36.);
/// Room the top bar leaves for the controls.
pub(crate) const CONTROLS_W: Pixels = px(132.);

/// The red Windows uses for a hovered close button.
fn close_hover() -> Rgba {
    rgb(0xc4_2b1c)
}

#[derive(Clone, Copy)]
struct Hover {
    fill: Hsla,
    text: Hsla,
}

fn button(
    id: &'static str,
    glyph: &'static str,
    area: WindowControlArea,
    cell_w: Pixels,
    hover: Hover,
    pal: Palette,
) -> impl IntoElement {
    let Hover { fill, text } = hover;
    h_flex()
        .id(id)
        .group(id)
        .w(cell_w)
        .h(px(HEADER_H - 1.))
        .pl(px(2.))
        .items_center()
        // A control over a drag region must win the hit test.
        .occlude()
        .window_control_area(area)
        .text_color(pal.text_muted)
        .hover(move |style| style.text_color(text))
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .w(CHIP_W)
                .h(px(CONTROL_H))
                .rounded(pal.control_radius)
                .font_family(GLYPH_FONT)
                .text_size(px(10.))
                .group_hover(id, move |style| style.bg(fill))
                .child(glyph),
        )
}

/// Minimize, maximize and close, pinned to the window's top-right corner so no
/// page layout can push them out of it.
pub(crate) fn window_controls(window: &Window, pal: Palette) -> impl IntoElement {
    let neutral = || Hover {
        fill: pal.control_hover,
        text: pal.text_primary,
    };
    h_flex()
        .absolute()
        .top_0()
        .right_0()
        .child(button(
            "caption-minimize",
            GLYPH_MINIMIZE,
            WindowControlArea::Min,
            CELL_W,
            neutral(),
            pal,
        ))
        .child(button(
            "caption-maximize",
            if window.is_maximized() {
                GLYPH_RESTORE
            } else {
                GLYPH_MAXIMIZE
            },
            WindowControlArea::Max,
            CELL_W,
            neutral(),
            pal,
        ))
        .child(button(
            "caption-close",
            GLYPH_CLOSE,
            WindowControlArea::Close,
            CORNER_CELL_W,
            Hover {
                fill: close_hover().into(),
                text: gpui::white(),
            },
            pal,
        ))
}

/// The drag strip for a frame with no top bar: loading, error and permission
/// screens. Titled like the taskbar entry.
pub(crate) fn caption_row(pal: Palette) -> impl IntoElement {
    h_flex()
        .id("caption-row")
        .flex_shrink_0()
        .h(px(HEADER_H))
        .px_5()
        .items_center()
        .border_b_1()
        .border_color(pal.border)
        .text_caption()
        .text_color(pal.text_muted)
        .window_control_area(WindowControlArea::Drag)
        .child("OpenLogi")
}
