//! The settings app's GPUI [`AssetSource`].
//!
//! Composes the ring glyphs every frontend shares ([`openlogi_ui::action_icons`])
//! with the two sources only this app draws from: its embedded logo, and
//! gpui-component's bundled lucide set behind `IconName`. The overlay links
//! neither, which is why they live here and not in the shared crate.

use std::borrow::Cow;
use std::sync::Arc;

use gpui::{AssetSource, Result, SharedString};
use openlogi_ui::action_icons::ActionIcons;

/// Asset path [`AppAssets`] resolves to the embedded app logo.
pub const LOGO: &str = "openlogi.png";

/// The 1024×1024 app icon, embedded into the binary.
const LOGO_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../design/icon/openlogi.png"
));

/// The 128×128 app icon behind [`window_icon`].
const WINDOW_ICON_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../design/icon/openlogi-128.png"
));

/// The window icon X11 publishes as `_NET_WM_ICON`, for window managers that
/// don't resolve it from the `.desktop` file (XFCE). Wayland has no such
/// property — the compositor looks the icon up by `app_id` — and GPUI ignores
/// the field elsewhere, so only Linux pays for the decode. 128 px rather than
/// the 1024 px logo: the property is raw ARGB, 4 MiB at full size.
pub fn window_icon() -> Option<Arc<image::RgbaImage>> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    match image::load_from_memory_with_format(WINDOW_ICON_BYTES, image::ImageFormat::Png) {
        Ok(icon) => Some(Arc::new(icon.into_rgba8())),
        Err(e) => {
            tracing::warn!("window icon failed to decode: {e}");
            None
        }
    }
}

/// GPUI asset source: the app logo, the shared ring glyphs, then
/// gpui-component's bundled icons for everything else.
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == LOGO {
            return Ok(Some(Cow::Borrowed(LOGO_BYTES)));
        }
        if let Some(bytes) = ActionIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_kit_assets::Assets.list(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn window_icon_decodes_at_its_published_size() {
        let icon = window_icon().expect("embedded window icon decodes");
        assert_eq!(icon.dimensions(), (128, 128));
    }

    #[test]
    fn logo_ring_and_kit_assets_remain_available() {
        let logo = AppAssets.load(LOGO).unwrap().unwrap();
        assert!(logo.starts_with(b"\x89PNG\r\n\x1a\n"));

        for path in [
            openlogi_ui::action_icons::RING_CANCEL_ICON,
            "icons/check.svg",
            "icons/chevron-down.svg",
        ] {
            let bytes = AppAssets.load(path).unwrap().unwrap();
            assert!(
                std::str::from_utf8(&bytes).unwrap().contains("<svg"),
                "missing SVG content for {path}"
            );
        }
    }
}
