//! The host architecture, spelled the way each Linux packaging tool wants it.
//!
//! The Linux artifacts are built natively (the release CI runs an amd64 and an
//! arm64 runner), so the host arch is the package arch. nfpm and AppImage name
//! the same two architectures differently; this is the one place that mapping
//! lives, so a third packager cannot introduce a third spelling of it.

use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HostArch {
    X86_64,
    Aarch64,
}

impl HostArch {
    /// The architecture this xtask binary was compiled for.
    pub(super) fn detect() -> Result<Self> {
        Self::from_rust(std::env::consts::ARCH)
    }

    /// Map a Rust `target_arch` name to a Linux package architecture.
    pub(super) fn from_rust(arch: &str) -> Result<Self> {
        match arch {
            "x86_64" => Ok(Self::X86_64),
            "aarch64" => Ok(Self::Aarch64),
            other => bail!("unsupported Linux package architecture: {other}"),
        }
    }

    /// nfpm's `arch:` value, stamped into `.deb`/`.rpm` metadata and names.
    pub(super) const fn nfpm(self) -> &'static str {
        match self {
            Self::X86_64 => "amd64",
            Self::Aarch64 => "arm64",
        }
    }

    /// appimagetool's `ARCH`, which also ends up in the `.AppImage` file name.
    pub(super) const fn appimage(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }
}
