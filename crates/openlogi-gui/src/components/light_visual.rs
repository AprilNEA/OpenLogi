//! Shared standalone-light visuals used by the gallery and detail screen.
//!
//! Known models can opt into source-owned product artwork. Unknown models keep
//! the protocol-neutral generated visual and never borrow another model's
//! image or diffuser geometry.

use std::sync::{Arc, LazyLock, Mutex};

use gpui::prelude::StyledImage as _;
use gpui::{
    AnyElement, BoxShadow, IntoElement, ObjectFit, ParentElement, RenderImage, Styled, div, hsla,
    img, point, px, relative,
};
use gpui_component::{Icon, IconName};
use image::{Frame, ImageBuffer, Rgba};
use openlogi_core::config::LightSettings;

use crate::app_assets::{LITRA_GLOW, LITRA_GLOW_BYTES};
use crate::theme::Palette;

// These bounds include the diffuser edge and a small amount of transparent
// padding. The mask inside the generated image is derived from the actual
// product pixels, so it remains aligned when the artwork is updated.
const DIFFUSER_MASK_LEFT: f32 = 0.237;
const DIFFUSER_MASK_TOP: f32 = 0.024;
const DIFFUSER_MASK_WIDTH: f32 = 0.526;
const DIFFUSER_MASK_HEIGHT: f32 = 0.526;
const DIFFUSER_CROP: (u32, u32, u32, u32) = (286, 25, 635, 545);

type DiffuserBitmap = ImageBuffer<Rgba<u8>, Vec<u8>>;
type DiffuserCacheEntry = (u16, u8, Arc<RenderImage>);

static DIFFUSER_SOURCE: LazyLock<Option<DiffuserBitmap>> = LazyLock::new(|| {
    let source = image::load_from_memory(LITRA_GLOW_BYTES).ok()?.into_rgba8();
    Some(
        image::imageops::crop_imm(
            &source,
            DIFFUSER_CROP.0,
            DIFFUSER_CROP.1,
            DIFFUSER_CROP.2,
            DIFFUSER_CROP.3,
        )
        .to_image(),
    )
});

static DIFFUSER_CACHE: LazyLock<Mutex<Option<DiffuserCacheEntry>>> =
    LazyLock::new(|| Mutex::new(None));

/// Render a standalone light inside a home-gallery image slot.
pub(crate) fn gallery(
    artwork: Option<&str>,
    online: bool,
    enabled: bool,
    settings: LightSettings,
    pal: Palette,
) -> AnyElement {
    if uses_glow_artwork(artwork) {
        visual_container()
            .child(product_image(210., 180., online, enabled, settings))
            .into_any_element()
    } else {
        generated_visual(210., 180., online, enabled, settings, pal).into_any_element()
    }
}

/// Render a standalone light as the large hero in its detail view.
pub(crate) fn detail(
    artwork: Option<&str>,
    online: bool,
    enabled: bool,
    settings: LightSettings,
    pal: Palette,
) -> gpui::Div {
    let content = if uses_glow_artwork(artwork) {
        product_image(536., 460., online, enabled, settings)
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
        .child(content)
}

fn uses_glow_artwork(artwork: Option<&str>) -> bool {
    artwork == Some(LITRA_GLOW)
}

/// Keep the product artwork as the source of truth and layer the power state on
/// top of the diffuser. The overlay is expressed as percentages of the PNG so
/// it stays aligned in both the gallery thumbnail and the detail hero.
fn product_image(
    width: f32,
    height: f32,
    online: bool,
    enabled: bool,
    settings: LightSettings,
) -> gpui::Div {
    let powered = online && enabled;
    let image_opacity = if online { 1. } else { 0.5 };
    let temperature = settings.temperature_kelvin.unwrap_or(4600);
    let mut product = div()
        .relative()
        .w(px(width))
        .h(px(height))
        .opacity(image_opacity)
        .child(img(LITRA_GLOW).size_full());

    if powered && let Some(mask) = diffuser_mask(temperature, settings.brightness_percent) {
        product = product.child(
            div()
                .absolute()
                .left(relative(DIFFUSER_MASK_LEFT))
                .top(relative(DIFFUSER_MASK_TOP))
                .w(relative(DIFFUSER_MASK_WIDTH))
                .h(relative(DIFFUSER_MASK_HEIGHT))
                .child(img(mask).object_fit(ObjectFit::Fill).size_full()),
        );
    }

    product
}

fn diffuser_mask(temperature: u16, brightness_percent: u8) -> Option<Arc<RenderImage>> {
    if let Ok(cache) = DIFFUSER_CACHE.lock()
        && let Some((cached_temperature, cached_brightness, image)) = cache.as_ref()
        && *cached_temperature == temperature
        && *cached_brightness == brightness_percent
    {
        return Some(Arc::clone(image));
    }

    let source = DIFFUSER_SOURCE.as_ref()?;
    let image = Arc::new(RenderImage::new([Frame::new(render_diffuser(
        source,
        temperature,
        brightness_percent,
    ))]));

    if let Ok(mut cache) = DIFFUSER_CACHE.lock() {
        *cache = Some((temperature, brightness_percent, Arc::clone(&image)));
    }

    Some(image)
}

fn render_diffuser(
    source: &DiffuserBitmap,
    temperature: u16,
    brightness_percent: u8,
) -> DiffuserBitmap {
    let (red, green, blue) = kelvin_to_rgb(temperature);
    let tint_alpha = 0.16 + f32::from(brightness_percent) * 0.0016;

    ImageBuffer::from_fn(source.width(), source.height(), |x, y| {
        let [source_red, source_green, source_blue, source_alpha] = source.get_pixel(x, y).0;
        let mask_alpha = diffuser_pixel_alpha(source_red, source_green, source_blue, source_alpha);
        let alpha = channel_byte(mask_alpha * tint_alpha);

        // GPUI RenderImage frames are BGRA, unlike image crate buffers.
        Rgba([
            channel_byte(blue),
            channel_byte(green),
            channel_byte(red),
            alpha,
        ])
    })
}

fn diffuser_pixel_alpha(red: u8, green: u8, blue: u8, alpha: u8) -> f32 {
    if alpha == 0 {
        return 0.;
    }

    // The source artwork has a cool blue rim around the emitting face. Keep
    // that rim from being painted over, while retaining anti-aliased pixels
    // at the true diffuser boundary and the subtle Logitech mark.
    let minimum_channel = f32::from(red.min(green).min(blue));
    let blue_bias = f32::from(blue.saturating_sub(red));
    let brightness = ((minimum_channel - 220.) / 35.).clamp(0., 1.);
    let neutral = ((24. - blue_bias) / 18.).clamp(0., 1.);
    f32::from(alpha) / 255. * brightness * neutral
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the normalized color channel is clamped to the byte range before conversion"
)]
fn channel_byte(channel: f32) -> u8 {
    (channel.clamp(0., 1.) * 255.).round() as u8
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

fn kelvin_to_rgb(kelvin: u16) -> (f32, f32, f32) {
    let temperature = (f32::from(kelvin).clamp(1000., 40_000.) / 100.).max(10.);
    let red = if temperature <= 66. {
        1.
    } else {
        (329.6987 * (temperature - 60.).powf(-0.133_204_76) / 255.).clamp(0., 1.)
    };
    let green = if temperature <= 66. {
        (99.4708 * temperature.ln() - 161.1196) / 255.
    } else {
        (288.1222 * (temperature - 60.).powf(-0.075_514_85) / 255.).clamp(0., 1.)
    };
    let blue = if temperature >= 66. {
        1.
    } else if temperature <= 19. {
        0.
    } else {
        (138.5177 * (temperature - 10.).ln() - 305.0448) / 255.
    };
    (red, green.clamp(0., 1.), blue.clamp(0., 1.))
}

#[cfg(test)]
mod tests {
    use super::{LITRA_GLOW, diffuser_pixel_alpha, kelvin_to_rgb, uses_glow_artwork};

    #[test]
    fn only_glow_artwork_uses_the_product_renderer() {
        assert!(uses_glow_artwork(Some(LITRA_GLOW)));
        assert!(!uses_glow_artwork(None));
        assert!(!uses_glow_artwork(Some("product-art/litra-beam/front.png")));
    }

    #[test]
    fn daylight_temperature_is_neutral() {
        let (red, green, blue) = kelvin_to_rgb(6500);
        assert!(red > 0.98);
        assert!(green > 0.96);
        assert!(blue > 0.94);
        assert!(red - blue < 0.06);
    }

    #[test]
    fn warm_temperature_is_amber() {
        let (red, green, blue) = kelvin_to_rgb(2700);
        assert!(red > green);
        assert!(green > blue);
    }

    #[test]
    fn diffuser_mask_ignores_transparency_and_cool_rim() {
        assert!(diffuser_pixel_alpha(255, 255, 255, 0).abs() <= f32::EPSILON);
        assert!(
            diffuser_pixel_alpha(250, 250, 250, 255) > diffuser_pixel_alpha(225, 230, 255, 255)
        );
    }
}
