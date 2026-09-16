//! Application icon integration.
//!
//! The bundle ships every alternate under `Contents/Resources/Icons`, compiled
//! by `cargo xtask macos icon`; applying one hands its `.icns` to macOS through
//! [`appicon`], which sets both the Dock tile of this process and the icon
//! Finder and Launchpad read. Nothing here is fatal: an icon that cannot be
//! applied — a bundle owned by another user, a build without the alternates —
//! leaves the app wearing what it was signed with, which is a cosmetic loss
//! rather than a reason to fail a launch.
//!
//! The profile switcher also resolves other installed applications through
//! Launch Services so their real Finder icons can identify per-app profiles.

use std::path::PathBuf;
use std::sync::Arc;

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

/// Resolve the installed application's icon for a profile identifier or app path.
///
/// Exact app paths use the icon for that file. Profile identifiers resolve
/// through the application registry. The lookup does blocking platform work —
/// callers run it on the background executor, never on the render path.
#[must_use]
pub fn application_icon(identifier: &str) -> Option<Arc<gpui::RenderImage>> {
    #[cfg(target_os = "macos")]
    {
        use appcatalog::{ApplicationIdentity, IdentityKind};

        /// Pixel edge of the fetched rendition: comfortably above the 18 pt
        /// display size at 2× scale, far below the 1024 px source renditions.
        const ICON_EDGE: u32 = 64;

        if let Some(icon) = openlogi_ui::application_icon::application_icon(identifier, ICON_EDGE) {
            return Some(icon);
        }
        let identity =
            ApplicationIdentity::new(IdentityKind::MacBundleIdentifier, identifier.to_string());
        let icon = match appcatalog::application_icon(&identity, ICON_EDGE) {
            Ok(icon) => icon?,
            Err(error) => {
                warn!(%identifier, %error, "could not render the application icon");
                return None;
            }
        };
        openlogi_ui::image::render_image_from_rgba(icon.width(), icon.height(), icon.into_rgba())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = identifier;
        None
    }
}

/// Resolve a macOS bundle identifier to its launch path.
///
/// The lookup is blocking and must run on the background executor.
#[must_use]
pub fn application_path(identifier: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        use objc2::rc::autoreleasepool;
        use objc2_app_kit::NSWorkspace;
        use objc2_foundation::NSString;

        autoreleasepool(|_| {
            let identifier = NSString::from_str(identifier);
            NSWorkspace::sharedWorkspace()
                .URLForApplicationWithBundleIdentifier(&identifier)?
                .path()
                .map(|path| path.to_string())
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = identifier;
        None
    }
}

/// Resolve `file` inside the bundle's icon directory, if it is there.
fn icons_dir(file: String) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.parent()?.parent()?.join("Resources/Icons").join(file);
    path.is_file().then_some(path)
}
