//! Standalone-light visuals used by the gallery and detail screen.
//!
//! Known models can opt into source-owned product artwork. Unknown models keep
//! the protocol-neutral generated visual and never borrow another model's
//! image or diffuser geometry.

use gpui::{AnyElement, BoxShadow, IntoElement, ParentElement, Styled, div, hsla, img, point, px};
use gpui_component::{Icon, IconName};
use openlogi_core::config::LightSettings;

use crate::services::assets::ResolvedAsset;
use crate::ui::theme::Palette;

/// Render a standalone light inside a home-gallery image slot.
pub(crate) fn gallery(
    asset: Option<&ResolvedAsset>,
    online: bool,
    enabled: bool,
    settings: LightSettings,
    pal: Palette,
) -> AnyElement {
    if let Some(asset) = asset {
        visual_container()
            .child(product_image(asset, 210., 180., online))
            .into_any_element()
    } else {
        generated_visual(210., 180., online, enabled, settings, pal).into_any_element()
    }
}

/// Render a standalone light as the large hero in its detail view.
pub(crate) fn detail(
    asset: Option<&ResolvedAsset>,
    online: bool,
    enabled: bool,
    settings: LightSettings,
    pal: Palette,
) -> gpui::Div {
    let content = if let Some(asset) = asset {
        product_image(asset, 536., 460., online)
    } else {
        generated_visual(536., 460., online, enabled, settings, pal)
    };
    visual_container()
        .flex_1()
        .min_w(px(440.))
        .h(px(520.))
        .rounded(pal.card_radius)
        .border_1()
        .border_color(pal.border)
        .bg(pal.surface)
        .overflow_hidden()
        .child(content)
}

fn product_image(asset: &ResolvedAsset, width: f32, height: f32, online: bool) -> gpui::Div {
    let image_opacity = if online { 1. } else { 0.5 };
    div()
        .relative()
        .flex()
        .items_center()
        .justify_center()
        .w(px(width))
        .h(px(height))
        .overflow_hidden()
        .opacity(image_opacity)
        // `size_full` lets transparent product artwork escape this slot when
        // its intrinsic aspect ratio differs from the slot. Keep the source
        // aspect ratio while bounding both dimensions to the slot.
        .child(img(asset.image_path.clone()).max_w_full().max_h_full())
}

fn visual_container() -> gpui::Div {
    div()
        .relative()
        .w_full()
        .flex()
        .items_center()
        .justify_center()
}

fn generated_visual(
    width: f32,
    height: f32,
    online: bool,
    enabled: bool,
    settings: LightSettings,
    pal: Palette,
) -> gpui::Div {
    let powered = online && enabled;
    let glow = light_color(settings.temperature_kelvin.unwrap_or(4600));
    let brightness = f32::from(settings.brightness_percent.min(100)) / 100.;
    let halo_size = width.min(height) * 0.56;
    let face_size = halo_size * 0.58;

    let halo = div()
        .size(px(halo_size))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(if powered {
            glow.opacity(0.08 + brightness * 0.18)
        } else {
            pal.surface_hover
        });
    let halo = if powered {
        halo.shadow(vec![BoxShadow {
            color: glow.opacity(0.12 + brightness * 0.2),
            offset: point(px(0.), px(0.)),
            blur_radius: px(34.),
            spread_radius: px(3.),
            inset: false,
        }])
    } else {
        halo
    };

    div()
        .w(px(width))
        .h(px(height))
        .flex()
        .items_center()
        .justify_center()
        .opacity(if online { 1. } else { 0.5 })
        .child(
            halo.child(
                div()
                    .size(px(face_size))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(if powered {
                        glow.opacity(0.55)
                    } else {
                        pal.border
                    })
                    .bg(if powered {
                        glow.opacity(0.2)
                    } else {
                        pal.surface
                    })
                    .child(Icon::new(IconName::Sun).size_12().text_color(if powered {
                        glow
                    } else {
                        pal.text_muted
                    })),
            ),
        )
}

fn light_color(kelvin: u16) -> gpui::Hsla {
    let normalized = (f32::from(kelvin.clamp(2700, 6500)) - 2700.) / 3800.;
    let hue = 0.09 + normalized * 0.05;
    let saturation = 0.9 - normalized * 0.48;
    hsla(hue, saturation, 0.68, 1.)
}
