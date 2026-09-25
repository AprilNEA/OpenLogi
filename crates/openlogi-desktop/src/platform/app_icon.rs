//! The icon this app wears.
//!
//! The bundle ships every alternate under `Contents/Resources/Icons` (built by
//! `cargo xtask macos icon`); [`appicon`] hands the chosen `.icns` to macOS,
//! which updates the Dock tile and what Finder and Launchpad show. Failure is
//! cosmetic — the app keeps the icon it was signed with — never fatal.

use std::path::PathBuf;
use std::sync::Arc;

use openlogi_core::config::AppIcon;
use tracing::{debug, warn};

/// Bundle-side behaviour of an [`AppIcon`]. `openlogi-core` owns the enum and
/// stays platform-free, so the effects live here as an extension trait.
pub trait AppIconExt {
    /// Re-apply the persisted choice at startup. The default touches nothing,
    /// so an icon the user pasted on in Finder survives; any other choice is
    /// re-applied every launch because an update replaces the bundle.
    fn restore(self);

    /// Apply a choice the user just made. Picking the default clears whatever
    /// the bundle is wearing.
    fn apply(self);

    /// The PNG Settings previews for this icon, `None` outside a bundle that
    /// ships it.
    #[must_use]
    fn preview(self) -> Option<PathBuf>;
}

impl AppIconExt for AppIcon {
    fn restore(self) {
        if self.is_default() {
            return;
        }
        self.apply();
    }

    fn apply(self) {
        let outcome = if self.is_default() {
            appicon::reset()
        } else {
            let Some(icns) = bundled_icon(self, "icns") else {
                warn!(icon = %self, "the bundle ships no icon by that name; leaving the icon alone");
                return;
            };
            appicon::set(appicon::Icon::File(&icns))
        };
        match outcome {
            Ok(()) => debug!(icon = %self, "app icon applied"),
            Err(error) => warn!(icon = %self, %error, "could not apply the app icon"),
        }
    }

    fn preview(self) -> Option<PathBuf> {
        bundled_icon(self, "png")
    }
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

/// `icon`'s `ext` file under the bundle's `Resources/Icons`, if it ships one.
fn bundled_icon(icon: AppIcon, ext: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe
        .parent()?
        .parent()?
        .join("Resources/Icons")
        .join(format!("{icon}.{ext}"));
    path.is_file().then_some(path)
}
