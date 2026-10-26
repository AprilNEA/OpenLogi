//! How OpenLogi's Linux release packages are named.
//!
//! `xtask linux package` hands [`arch_tag`] to nfpm, the release workflow
//! publishes each package as [`release_file_name`] (`.github/workflows/build.yml`),
//! `packaging/linux/install.sh` downloads that name, and the desktop app picks
//! that asset out of a GitHub release. The Rust consumers derive both here, so
//! the package xtask builds and the asset the app looks for cannot drift apart.

use std::fmt::Display;

/// The package arch tag (Debian's and nfpm's naming) for a Rust target arch
/// such as [`std::env::consts::ARCH`], or `None` for an arch OpenLogi ships no
/// Linux package for.
#[must_use]
pub fn arch_tag(rust_arch: &str) -> Option<&'static str> {
    match rust_arch {
        "x86_64" => Some("amd64"),
        "aarch64" => Some("arm64"),
        _ => None,
    }
}

/// The published file name of the `version` package for `arch_tag` (from
/// [`arch_tag`]) in the format with file `extension` (`deb`, `rpm`,
/// `pkg.tar.zst`): `openlogi-v<version>-linux-<arch>.<extension>`.
#[must_use]
pub fn release_file_name(version: impl Display, arch_tag: &str, extension: &str) -> String {
    format!("openlogi-v{version}-linux-{arch_tag}.{extension}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_shipped_arches_and_nothing_else() {
        assert_eq!(arch_tag("x86_64"), Some("amd64"));
        assert_eq!(arch_tag("aarch64"), Some("arm64"));
        assert_eq!(arch_tag("riscv64"), None);
    }

    #[test]
    fn release_file_name_matches_the_published_assets() {
        assert_eq!(
            release_file_name("0.8.15", "amd64", "deb"),
            "openlogi-v0.8.15-linux-amd64.deb"
        );
        assert_eq!(
            release_file_name("0.8.19-beta.2", "arm64", "pkg.tar.zst"),
            "openlogi-v0.8.19-beta.2-linux-arm64.pkg.tar.zst"
        );
    }
}
