//! The complete [Lucide](https://lucide.dev) icon catalog (ISC license,
//! `assets/lucide/LICENSE-LUCIDE`), for the symbols a user puts on a keypad's
//! LCD keys.
//!
//! One owner for both sides: the agent rasterizes a chosen icon onto the key,
//! and the settings app lists and previews the catalog in its picker. The
//! SVGs are Lucide 1.43.0, the set gpui-kit 0.8.1 ships; this crate keeps its
//! own copy because the agent links it without GPUI. It sits
//! behind the `lucide` feature so the overlay, which never draws them, does
//! not carry the catalog.

use std::borrow::Cow;

#[derive(rust_embed::RustEmbed)]
#[folder = "assets/lucide"]
#[include = "*.svg"]
struct Catalog;

/// The asset-path prefix the settings app resolves Lucide icons under, so
/// `lucide/camera.svg` names the `camera` icon.
pub const ASSET_PREFIX: &str = "lucide/";

/// Whether `name` is shaped like a Lucide icon name (kebab-case ASCII). A
/// name from a hand-edited config is checked before it reaches a path.
fn is_icon_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The SVG of the Lucide icon `name`, or `None` when the catalog has none.
#[must_use]
pub fn svg(name: &str) -> Option<Cow<'static, [u8]>> {
    if !is_icon_name(name) {
        return None;
    }
    Catalog::get(&format!("{name}.svg")).map(|file| file.data)
}

/// The asset path the settings app draws the icon `name` from.
#[must_use]
pub fn asset_path(name: &str) -> String {
    format!("{ASSET_PREFIX}{name}.svg")
}

/// The SVG behind a settings-app asset `path` under [`ASSET_PREFIX`].
#[must_use]
pub fn svg_for_asset(path: &str) -> Option<Cow<'static, [u8]>> {
    let name = path.strip_prefix(ASSET_PREFIX)?.strip_suffix(".svg")?;
    svg(name)
}

/// Every icon name in the catalog, sorted.
#[must_use]
pub fn names() -> Vec<String> {
    let mut names: Vec<String> = Catalog::iter()
        .filter_map(|file| file.strip_suffix(".svg").map(str::to_owned))
        .collect();
    names.sort_unstable();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_is_complete_and_resolves_by_name_and_path() {
        let names = names();
        assert!(names.len() > 1_500, "only {} icons", names.len());
        assert!(names.iter().any(|name| name == "camera"));
        let camera = svg("camera").expect("camera icon");
        assert!(String::from_utf8_lossy(&camera).contains("currentColor"));
        assert_eq!(asset_path("camera"), "lucide/camera.svg");
        assert!(svg_for_asset("lucide/camera.svg").is_some());
        assert!(svg_for_asset("icons/camera.svg").is_none());
    }

    #[test]
    fn malformed_names_never_reach_the_catalog() {
        for name in ["", "../secrets", "Camera", "camera.svg", "a b"] {
            assert!(svg(name).is_none(), "{name:?}");
        }
    }
}
