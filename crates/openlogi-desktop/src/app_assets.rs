//! The settings app's GPUI [`AssetSource`].
//!
//! Composes the ring glyphs every frontend shares ([`openlogi_ui::action_icons`])
//! with the sources only this app draws from: its embedded logo, the full
//! Lucide catalog a keypad key's symbol is picked from
//! ([`openlogi_ui::lucide`], under `lucide/<name>.svg`), and gpui-component's
//! bundled lucide set behind `IconName`. The overlay links none of these,
//! which is why they live here and not in the shared crate.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};
use openlogi_ui::action_icons::ActionIcons;

/// Asset path [`AppAssets`] resolves to the embedded app logo.
pub const LOGO: &str = "openlogi.png";

/// The 512×512 export of the app icon, embedded into the binary.
///
/// Not the 1024×1024 master: GPUI draws an image by sampling its
/// full-resolution texture bilinearly, without mipmaps, so a source many times
/// larger than the drawn size aliases rather than looking sharper. The largest
/// draw is the 96 pt About logo, 288 px at 3×; 512 px covers that and keeps
/// 0.5 MB out of the binary.
const LOGO_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../design/icon/openlogi-512.png"
));

/// GPUI asset source: the app logo, the shared ring glyphs, the keypad
/// symbol catalog under [`openlogi_ui::lucide::ASSET_PREFIX`], then
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
        if path.starts_with(openlogi_ui::lucide::ASSET_PREFIX) {
            return Ok(openlogi_ui::lucide::svg_for_asset(path));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_kit::assets::Assets.list(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_ring_lucide_and_kit_assets_remain_available() {
        let logo = AppAssets.load(LOGO).unwrap().unwrap();
        assert!(logo.starts_with(b"\x89PNG\r\n\x1a\n"));

        for path in [
            openlogi_ui::action_icons::RING_CANCEL_ICON,
            "icons/check.svg",
            "icons/chevron-down.svg",
            crate::features::audio::AUDIO_ICON,
            "lucide/camera.svg",
        ] {
            let bytes = AppAssets.load(path).unwrap().unwrap();
            assert!(
                std::str::from_utf8(&bytes).unwrap().contains("<svg"),
                "missing SVG content for {path}"
            );
        }
    }

    #[test]
    fn an_unknown_keypad_symbol_resolves_to_nothing() {
        assert!(AppAssets.load("lucide/no-such-icon.svg").unwrap().is_none());
    }
}
