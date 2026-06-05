//! The app's GPUI [`AssetSource`].
//!
//! Serves the embedded OpenLogi logo and delegates every other path to
//! gpui-component's icon assets (the lucide SVGs behind `IconName`). Embedding
//! the logo via `include_bytes!` means `img("openlogi.png")` resolves the same
//! inside a packaged `.app` as it does from a dev build — a filesystem path
//! would not.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

/// Asset path [`AppAssets`] resolves to the embedded app logo.
pub const LOGO: &str = "openlogi.png";

/// The 1024×1024 app icon, embedded into the binary.
const LOGO_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../design/icon/openlogi.png"
));

/// GPUI asset source: the embedded logo plus gpui-component's bundled icons.
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == LOGO {
            return Ok(Some(Cow::Borrowed(LOGO_BYTES)));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_component_assets::Assets.list(path)
    }
}
