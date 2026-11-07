//! Per-key colors on the keyboard's own photo.
//!
//! A keyboard whose zone map is known ([`per_key_zones`]) and whose depot
//! draws its keys ([`openlogi_assets::Metadata::key_regions`]) shows its
//! photo in the Lighting tab with every key clickable. Each key's color
//! glows behind the photo, so it reads through the gaps between the caps the
//! way the backlight does, and a key with a color of its own carries a tint
//! over its cap.

use std::collections::BTreeSet;
use std::path::PathBuf;

use gpui::{
    Context, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _, div, img, prelude::FluentBuilder as _, px,
};
use openlogi_assets::KeyRegion;
use openlogi_core::color::Rgb;
use openlogi_core::config::{Lighting, LightingKey};
use openlogi_core::device::DeviceKind;
use openlogi_device_registry::keyboard::per_key_zones;

use super::keyboard_rgb::LightingPanel;
use crate::state::DeviceRecord;
use crate::ui::theme;

/// The width the photo is drawn at. The Lighting tab widens to fit it.
pub(crate) const PHOTO_W: f32 = 840.;
/// How far a key's glow reaches past its cap, in drawn pixels.
const GLOW_SPREAD: f32 = 4.;
const GLOW_OPACITY: f32 = 0.85;
/// The tint over a cap whose key has a color of its own.
const TINT_OPACITY: f32 = 0.3;

/// The keyboard photo and where each key sits on it.
pub(crate) struct PerKeyPhoto {
    image_path: PathBuf,
    /// The photo's size in the depot's pixels, which the key boxes use.
    origin: (f32, f32),
    /// Keys in paint order: outline boxes first, so a plain key over one
    /// (the `#` inside an ISO Enter's box) is drawn on top and takes the
    /// click.
    keys: Vec<(LightingKey, KeyRegion)>,
}

impl PerKeyPhoto {
    /// The keys' names in paint order.
    #[cfg(test)]
    pub(crate) fn key_order(&self) -> Vec<&'static str> {
        self.keys.iter().map(|(key, _)| key.name()).collect()
    }
}

/// The photo editor for `record`, or `None` when the keyboard has no known
/// zone map or its depot does not draw its keys.
pub(crate) fn per_key_photo(record: &DeviceRecord) -> Option<PerKeyPhoto> {
    if record.kind != DeviceKind::Keyboard {
        return None;
    }
    per_key_zones(record.model_info.as_ref()?.model_ids)?;
    let asset = record.asset.as_ref()?;
    let origin = asset.metadata.origin()?;
    let width = f32::from(u16::try_from(origin.width).ok().filter(|w| *w > 0)?);
    let height = f32::from(u16::try_from(origin.height).ok().filter(|h| *h > 0)?);
    let mut keys: Vec<_> = asset
        .metadata
        .key_regions()
        .into_iter()
        .filter_map(|region| Some((LightingKey::from_usage(region.usage)?, region)))
        .collect();
    if keys.is_empty() {
        return None;
    }
    keys.sort_by_key(|(_, region)| !region.outline);
    Some(PerKeyPhoto {
        image_path: asset.image_path.clone(),
        origin: (width, height),
        keys,
    })
}

/// The color `key` shows: its own, else the keyboard's.
pub(crate) fn key_color(lighting: &Lighting, key: LightingKey) -> Rgb {
    lighting.keys.get(&key).copied().unwrap_or(lighting.color)
}

/// The photo with each key's glow behind it and a click target over each
/// key, which toggles it in `selected`.
pub(crate) fn photo_editor(
    photo: &PerKeyPhoto,
    lighting: &Lighting,
    selected: &BTreeSet<LightingKey>,
    cx: &mut Context<LightingPanel>,
) -> impl IntoElement {
    let scale = PHOTO_W / photo.origin.0;
    let height = photo.origin.1 * scale;

    let glows: Vec<_> = if lighting.enabled {
        photo
            .keys
            .iter()
            .map(|&(key, region)| {
                let rect = region.rect;
                div()
                    .absolute()
                    .left(px(rect.x * scale - GLOW_SPREAD))
                    .top(px(rect.y * scale - GLOW_SPREAD))
                    .w(px(rect.width * scale + 2. * GLOW_SPREAD))
                    .h(px(rect.height * scale + 2. * GLOW_SPREAD))
                    .rounded(px(6.))
                    .bg(with_alpha(key_color(lighting, key), GLOW_OPACITY))
            })
            .collect()
    } else {
        Vec::new()
    };

    let targets: Vec<_> = photo
        .keys
        .iter()
        .map(|&(key, region)| {
            let rect = region.rect;
            let is_selected = selected.contains(&key);
            let own_color = lighting.keys.get(&key).copied();
            div()
                .id(("per-key", usize::from(key.usage())))
                .debug_selector(move || format!("per-key-{}", key.name()))
                .role(Role::Button)
                .aria_label(key.name())
                .absolute()
                .left(px(rect.x * scale))
                .top(px(rect.y * scale))
                .w(px(rect.width * scale))
                .h(px(rect.height * scale))
                .rounded(px(4.))
                .cursor_pointer()
                .border_2()
                .border_color(if is_selected {
                    theme::accent()
                } else {
                    gpui::transparent_black()
                })
                .when_some(own_color, |target, color| {
                    target.bg(with_alpha(color, TINT_OPACITY))
                })
                .when(!is_selected, |target| {
                    target.hover(|style| style.border_color(theme::accent_tint_hover()))
                })
                .on_click(cx.listener(move |panel, _event, _window, cx| {
                    panel.toggle_key(key);
                    cx.notify();
                }))
        })
        .collect();

    div()
        .relative()
        .flex_none()
        .w(px(PHOTO_W))
        .h(px(height))
        .children(glows)
        .child(
            img(photo.image_path.clone())
                .absolute()
                .top_0()
                .left_0()
                .w(px(PHOTO_W))
                .h(px(height)),
        )
        .children(targets)
}

fn with_alpha(color: Rgb, a: f32) -> Hsla {
    let (r, g, b) = color.components();
    gpui::Rgba {
        r: f32::from(r) / 255.,
        g: f32::from(g) / 255.,
        b: f32::from(b) / 255.,
        a,
    }
    .into()
}
