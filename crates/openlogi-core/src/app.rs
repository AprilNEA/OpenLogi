//! The foreground application, in the form per-app profiles match against.
//!
//! Lives in the platform-free core crate because three layers need the same
//! shape and none of them can see the others: `openlogi-hook` reads it from the
//! window server, `openlogi-agent-core` holds it, and `openlogi-ipc` puts it on
//! the wire. That last one makes this a wire type — see
//! `crates/openlogi-ipc/AGENTS.md`.

use serde::{Deserialize, Serialize};

/// One application, named the way a per-app profile names it.
///
/// [`Self::id`] is the whole of the matching contract; [`Self::display_name`]
/// exists only so a UI never has to show a reverse-DNS string to a human.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForegroundApp {
    /// The exact string a per-app profile key is compared against: a macOS
    /// bundle identifier, an X11 `WM_CLASS` class or a Wayland xdg `app_id` on
    /// Linux, or the lower-cased executable path on Windows.
    ///
    /// Those namespaces do not map onto one another by any simple string rule,
    /// which is why a profile authored under one of them will not match under
    /// another. See
    /// [`Config::effective_bindings`](crate::config::Config::effective_bindings)
    /// for the matcher this feeds.
    pub id: String,
    /// Human-readable name for the UI. Equal to [`Self::id`] on the platforms
    /// that report no name of their own.
    pub display_name: String,
}

impl ForegroundApp {
    /// An application the platform identified but did not name — the display
    /// name falls back to the identifier, which on those platforms (X11's
    /// `WM_CLASS`, a Wayland `app_id`) is already close to readable.
    #[must_use]
    pub fn unnamed(id: String) -> Self {
        Self {
            display_name: id.clone(),
            id,
        }
    }
}
