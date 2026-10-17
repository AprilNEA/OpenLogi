//! Read-only installation ownership of the running GUI, not its download history.
//!
//! Detection runs once off the UI thread. A package receipt must identify this
//! executable (or its enclosing app), not merely another installed OpenLogi.
//! Unmarked copies stay unknown; an app bundle is not proof of a DMG download.

#[cfg(any(target_os = "macos", target_os = "windows", test))]
use std::path::Path;

use gpui::{App, Global};

#[cfg(any(target_os = "linux", test))]
mod linux;
#[cfg(any(target_os = "macos", all(test, unix)))]
mod macos;
#[cfg(any(target_os = "windows", test))]
mod windows;

/// Evidence-backed ownership of the currently running copy.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "each host constructs only its own installation sources"
    )
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallationSource {
    /// A Homebrew receipt and Caskroom back-link identify this app bundle.
    Homebrew(HomebrewCask),
    /// The package database owns this executable as part of `openlogi`.
    LinuxPackage(LinuxPackage),
    /// The resolved executable lives in the Nix store.
    Nix,
    /// The per-user MSI registration points to this executable.
    WindowsMsi,
    /// This copy carries the portable ZIP's explicit distribution marker.
    WindowsPortable,
    /// A macOS bundle without matching Homebrew ownership; origin is unknown.
    MacAppBundle,
    /// No supported ownership evidence, unavailable metadata, or conflicting receipts.
    Unknown,
}

/// The two supported Homebrew cask tokens, independent of their recorded version.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(
        dead_code,
        reason = "Homebrew variants are constructed on macOS or in tests"
    )
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomebrewCask {
    /// `openlogi`, distributed by the official Homebrew cask repository.
    Official,
    /// `openlogi@latest`, distributed by `aprilnea/tap`.
    Latest,
}

/// Linux package database that owns the running executable.
#[cfg_attr(
    all(not(target_os = "linux"), not(test)),
    expect(
        dead_code,
        reason = "Linux package variants are constructed on Linux or in tests"
    )
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxPackage {
    /// Debian-family package registered with dpkg.
    Deb,
    /// RPM package registered with rpm.
    Rpm,
    /// Arch package registered with pacman.
    Arch,
}

/// Startup detection state; pending is distinct from an inconclusive result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installation {
    /// The one-time background probe has not completed yet.
    Detecting,
    /// The probe completed, possibly without conclusive ownership evidence.
    Detected(InstallationSource),
}

impl Global for Installation {}

impl InstallationSource {
    /// Inspect this process's executable. Does no network I/O or installation writes.
    pub fn detect() -> Self {
        let Ok(executable) = std::env::current_exe().and_then(std::fs::canonicalize) else {
            return Self::Unknown;
        };
        #[cfg(target_os = "linux")]
        return linux::detect(&executable);
        #[cfg(target_os = "macos")]
        return macos::detect(&executable);
        #[cfg(target_os = "windows")]
        return windows::detect(&executable);
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            let _ = executable;
            Self::Unknown
        }
    }
}

/// Publish the installation state and start its one-time, off-main-thread probe.
pub fn install(cx: &mut App) {
    cx.set_global(Installation::Detecting);
    cx.spawn(async move |cx| {
        let source = cx
            .background_executor()
            .spawn(async { InstallationSource::detect() })
            .await;
        tracing::info!(?source, "detected installation source");
        cx.update(|cx| cx.set_global(Installation::Detected(source)));
    })
    .detach();
}

/// Compare existing filesystem objects after resolving links; failures never match.
#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}
