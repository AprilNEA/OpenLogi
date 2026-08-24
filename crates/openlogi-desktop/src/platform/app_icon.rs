//! Wearing the icon the user picked.
//!
//! The bundle ships every alternate under `Contents/Resources/Icons`, compiled
//! by `cargo xtask macos icon`; applying one hands its `.icns` to macOS through
//! [`appicon`], which sets both the Dock tile of this process and the icon
//! Finder and Launchpad read. Nothing here is fatal: an icon that cannot be
//! applied — a bundle owned by another user, a build without the alternates —
//! leaves the app wearing what it was signed with, which is a cosmetic loss
//! rather than a reason to fail a launch.

use std::path::PathBuf;

use openlogi_core::config::AppIcon;
use tracing::{debug, warn};

/// Re-apply the persisted choice at startup.
///
/// The default is applied by touching *nothing*. The bundle already wears it,
/// and a user who pasted their own icon onto the app in Finder gets to keep it
/// — an app that resets its icon on every launch quietly overwrites that, which
/// is the complaint Arc's icon picker collects.
///
/// A non-default choice is re-applied every launch on purpose: replacing the
/// bundle drops the icon, and an update does exactly that.
pub fn restore(icon: AppIcon) {
    if icon.is_default() {
        return;
    }
    apply(icon);
}

/// Apply a choice the user just made, the default included — picking it back is
/// an explicit request to drop whatever icon the bundle is wearing.
pub fn apply(icon: AppIcon) {
    let outcome = if icon.is_default() {
        appicon::reset()
    } else {
        let Some(icns) = alternate(icon) else {
            warn!(%icon, "the bundle ships no icon by that name; leaving the icon alone");
            return;
        };
        appicon::set(appicon::Icon::File(&icns))
    };
    match outcome {
        Ok(()) => debug!(%icon, "app icon applied"),
        Err(error) => warn!(%icon, %error, "could not apply the app icon"),
    }
}

/// The alternate's `.icns` inside this app bundle, `None` when the running
/// binary is not in one that ships it.
fn alternate(icon: AppIcon) -> Option<PathBuf> {
    icons_dir(format!("{icon}.icns"))
}

/// The render Settings draws for `icon`, `None` outside a bundle that ships it.
/// Every icon has one, the default included — the picker offers them all.
#[must_use]
pub fn preview(icon: AppIcon) -> Option<PathBuf> {
    icons_dir(format!("{icon}.png"))
}

/// Resolve `file` inside the bundle's icon directory, if it is there.
fn icons_dir(file: String) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.parent()?.parent()?.join("Resources/Icons").join(file);
    path.is_file().then_some(path)
}
