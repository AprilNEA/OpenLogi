//! The vendored Actions Ring glyphs, as a GPUI [`AssetSource`].
//!
//! Both frontends draw ring slots, so both need these bytes; nothing else in
//! either app's artwork is shared, which is why this source serves *only*
//! `action-icons/` and reports every other path as absent. A frontend needing
//! more composes this with its own source rather than growing this one —
//! anything added here is added to the overlay too.
//!
//! Embedding via `include_bytes!` means the paths resolve identically inside a
//! packaged `.app` and from a dev build; a filesystem path would not.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

/// Vendored [lucide](https://lucide.dev) icons (ISC license) for the binding
/// menus, embedded so they resolve identically in a packaged `.app` and a dev
/// build. Served under the `action-icons/` path prefix and rendered by
/// the settings action picker via `svg().path(..)`. These are
/// command glyphs (paste / cut / volume / lock / …) plus a couple of About-page
/// icons (changelog, bug) that gpui-component's bundled `IconName` set does not
/// cover.
#[rustfmt::skip]
const ACTION_ICONS: &[(&str, &[u8])] = &[
    ("action-icons/arrow-left.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/arrow-left.svg"))),
    ("action-icons/arrow-right.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/arrow-right.svg"))),
    ("action-icons/ban.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/ban.svg"))),
    ("action-icons/bell.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/bell.svg"))),
    ("action-icons/bluetooth.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/bluetooth.svg"))),
    ("action-icons/book-open.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/book-open.svg"))),
    ("action-icons/bolt.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/bolt.svg"))),
    ("action-icons/bug.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/bug.svg"))),
    ("action-icons/calendar.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/calendar.svg"))),
    ("action-icons/camera.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/camera.svg"))),
    ("action-icons/chevron-left.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/chevron-left.svg"))),
    ("action-icons/chevron-right.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/chevron-right.svg"))),
    ("action-icons/chevrons-down.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/chevrons-down.svg"))),
    ("action-icons/chevrons-left.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/chevrons-left.svg"))),
    ("action-icons/chevrons-right.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/chevrons-right.svg"))),
    ("action-icons/chevrons-up.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/chevrons-up.svg"))),
    ("action-icons/circle-arrow-left.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/circle-arrow-left.svg"))),
    ("action-icons/circle-arrow-right.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/circle-arrow-right.svg"))),
    ("action-icons/circle-dot.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/circle-dot.svg"))),
    ("action-icons/clipboard-paste.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/clipboard-paste.svg"))),
    ("action-icons/copy.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/copy.svg"))),
    ("action-icons/file.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/file.svg"))),
    ("action-icons/folder.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/folder.svg"))),
    ("action-icons/gauge.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/gauge.svg"))),
    ("action-icons/globe.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/globe.svg"))),
    ("action-icons/grid-3x3.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/grid-3x3.svg"))),
    ("action-icons/heart.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/heart.svg"))),
    ("action-icons/keyboard.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/keyboard.svg"))),
    ("action-icons/layers.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/layers.svg"))),
    ("action-icons/layout-grid.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/layout-grid.svg"))),
    ("action-icons/list-checks.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/list-checks.svg"))),
    ("action-icons/lock.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/lock.svg"))),
    ("action-icons/monitor.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/monitor.svg"))),
    ("action-icons/mouse-pointer-click.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/mouse-pointer-click.svg"))),
    ("action-icons/mouse.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/mouse.svg"))),
    ("action-icons/move.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/move.svg"))),
    ("action-icons/palette.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/palette.svg"))),
    ("action-icons/pencil.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/pencil.svg"))),
    ("action-icons/play.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/play.svg"))),
    ("action-icons/redo-2.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/redo-2.svg"))),
    ("action-icons/refresh-cw.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/refresh-cw.svg"))),
    ("action-icons/rotate-ccw.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/rotate-ccw.svg"))),
    ("action-icons/rotate-cw.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/rotate-cw.svg"))),
    ("action-icons/save.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/save.svg"))),
    ("action-icons/scissors.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/scissors.svg"))),
    ("action-icons/scroll-text.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/scroll-text.svg"))),
    ("action-icons/search.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/search.svg"))),
    ("action-icons/settings.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/settings.svg"))),
    ("action-icons/skip-back.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/skip-back.svg"))),
    ("action-icons/skip-forward.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/skip-forward.svg"))),
    ("action-icons/square-arrow-left.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/square-arrow-left.svg"))),
    ("action-icons/square-arrow-right.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/square-arrow-right.svg"))),
    ("action-icons/square-plus.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/square-plus.svg"))),
    ("action-icons/square-terminal.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/square-terminal.svg"))),
    ("action-icons/square-x.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/square-x.svg"))),
    ("action-icons/terminal.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/terminal.svg"))),
    ("action-icons/star.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/star.svg"))),
    ("action-icons/undo-2.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/undo-2.svg"))),
    ("action-icons/unifying.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/unifying.svg"))),
    ("action-icons/usb.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/usb.svg"))),
    ("action-icons/user.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/user.svg"))),
    ("action-icons/volume-1.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/volume-1.svg"))),
    ("action-icons/volume-2.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/volume-2.svg"))),
    ("action-icons/volume-x.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/action-icons/volume-x.svg"))),
];

/// GPUI asset source for the embedded ring glyphs, and nothing else.
pub struct ActionIcons;

impl AssetSource for ActionIcons {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ACTION_ICONS
            .iter()
            .find(|(candidate, _)| *candidate == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ACTION_ICONS
            .iter()
            .filter(|(candidate, _)| candidate.starts_with(path))
            .map(|(candidate, _)| SharedString::from(*candidate))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::binding::ActionRingIcon;

    use super::*;

    #[test]
    fn every_ring_gallery_icon_is_embedded() {
        for icon in ActionRingIcon::ALL {
            let loaded = ActionIcons.load(icon.asset_path());
            assert!(
                matches!(loaded, Ok(Some(_))),
                "missing embedded asset for {icon:?}"
            );
        }
    }
}
