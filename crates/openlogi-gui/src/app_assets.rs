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

/// Vendored [lucide](https://lucide.dev) icons (ISC license) for the binding
/// menus, embedded so they resolve identically in a packaged `.app` and a dev
/// build. Served under the `action-icons/` path prefix and rendered by
/// `mouse_model::picker::action_icon_path` via `svg().path(..)`. These are
/// command glyphs (paste / cut / volume / lock / …) that gpui-component's
/// bundled `IconName` set (UI chrome only) does not cover.
#[rustfmt::skip]
const ACTION_ICONS: &[(&str, &[u8])] = &[
    ("action-icons/arrow-left.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/arrow-left.svg"))),
    ("action-icons/arrow-right.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/arrow-right.svg"))),
    ("action-icons/ban.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/ban.svg"))),
    ("action-icons/camera.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/camera.svg"))),
    ("action-icons/chevron-left.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/chevron-left.svg"))),
    ("action-icons/chevron-right.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/chevron-right.svg"))),
    ("action-icons/chevrons-down.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/chevrons-down.svg"))),
    ("action-icons/chevrons-left.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/chevrons-left.svg"))),
    ("action-icons/chevrons-right.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/chevrons-right.svg"))),
    ("action-icons/chevrons-up.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/chevrons-up.svg"))),
    ("action-icons/circle-arrow-left.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/circle-arrow-left.svg"))),
    ("action-icons/circle-arrow-right.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/circle-arrow-right.svg"))),
    ("action-icons/clipboard-paste.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/clipboard-paste.svg"))),
    ("action-icons/copy.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/copy.svg"))),
    ("action-icons/gauge.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/gauge.svg"))),
    ("action-icons/grid-3x3.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/grid-3x3.svg"))),
    ("action-icons/keyboard.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/keyboard.svg"))),
    ("action-icons/layers.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/layers.svg"))),
    ("action-icons/layout-grid.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/layout-grid.svg"))),
    ("action-icons/list-checks.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/list-checks.svg"))),
    ("action-icons/lock.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/lock.svg"))),
    ("action-icons/monitor.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/monitor.svg"))),
    ("action-icons/mouse-pointer-click.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/mouse-pointer-click.svg"))),
    ("action-icons/mouse.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/mouse.svg"))),
    ("action-icons/move.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/move.svg"))),
    ("action-icons/play.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/play.svg"))),
    ("action-icons/redo-2.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/redo-2.svg"))),
    ("action-icons/refresh-cw.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/refresh-cw.svg"))),
    ("action-icons/rotate-ccw.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/rotate-ccw.svg"))),
    ("action-icons/rotate-cw.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/rotate-cw.svg"))),
    ("action-icons/save.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/save.svg"))),
    ("action-icons/scissors.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/scissors.svg"))),
    ("action-icons/search.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/search.svg"))),
    ("action-icons/skip-back.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/skip-back.svg"))),
    ("action-icons/skip-forward.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/skip-forward.svg"))),
    ("action-icons/square-arrow-left.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/square-arrow-left.svg"))),
    ("action-icons/square-arrow-right.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/square-arrow-right.svg"))),
    ("action-icons/square-plus.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/square-plus.svg"))),
    ("action-icons/square-x.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/square-x.svg"))),
    ("action-icons/undo-2.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/undo-2.svg"))),
    ("action-icons/volume-1.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/volume-1.svg"))),
    ("action-icons/volume-2.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/volume-2.svg"))),
    ("action-icons/volume-x.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/volume-x.svg"))),
];

/// GPUI asset source: the embedded logo + vendored action icons, then
/// gpui-component's bundled icons for everything else.
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == LOGO {
            return Ok(Some(Cow::Borrowed(LOGO_BYTES)));
        }
        if let Some((_, bytes)) = ACTION_ICONS.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(*bytes)));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_component_assets::Assets.list(path)
    }
}
