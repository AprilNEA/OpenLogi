//! Popover section categories for the action catalog.

/// Grouping for popover section headers.
///
/// Used by [`Action::category`](crate::binding::Action::category) and rendered
/// as a small muted label above each group in the action picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    /// Cut, copy, paste, undo, redo, select-all, find, save.
    Editing,
    /// Browser navigation: tabs, page reload, back/forward.
    Browser,
    /// Playback and volume controls.
    Media,
    /// Physical mouse clicks.
    Mouse,
    /// DPI cycle and SmartShift.
    Dpi,
    /// Scroll direction shortcuts.
    Scroll,
    /// Window/app navigation: Mission Control, Launchpad, etc.
    Navigation,
    /// Lock screen, show desktop, system-level actions.
    System,
}

impl Category {
    /// Human-readable English label for logs and non-localized contexts.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Category::Editing => "Editing",
            Category::Browser => "Browser",
            Category::Media => "Media",
            Category::Mouse => "Mouse",
            Category::Dpi => "DPI",
            Category::Scroll => "Scroll",
            Category::Navigation => "Navigation",
            Category::System => "System",
        }
    }

    /// Stable catalog key for the localized popover section header.
    #[must_use]
    pub fn translation_key(self) -> &'static str {
        match self {
            Category::Editing => "actions.editing",
            Category::Browser => "actions.browser",
            Category::Media => "actions.media",
            Category::Mouse => "device.mouse",
            Category::Dpi => "pointer.dpi",
            Category::Scroll => "pointer.scroll",
            Category::Navigation => "actions.navigation",
            Category::System => "actions.system",
        }
    }
}
