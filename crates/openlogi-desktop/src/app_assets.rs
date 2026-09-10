//! The settings app's GPUI [`AssetSource`].
//!
//! Composes the ring glyphs every frontend shares ([`openlogi_ui::action_icons`])
//! with the two sources only this app draws from: its embedded logo, and
//! gpui-component's bundled lucide set behind `IconName`. The overlay links
//! neither, which is why they live here and not in the shared crate.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};
use openlogi_ui::action_icons::ActionIcons;

/// Asset path [`AppAssets`] resolves to the embedded app logo.
pub const LOGO: &str = "openlogi.png";

/// The 1024×1024 app icon, embedded into the binary.
const LOGO_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../design/icon/openlogi.png"
));

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

    #[test]
    fn logo_ring_and_component_assets_remain_available() {
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
