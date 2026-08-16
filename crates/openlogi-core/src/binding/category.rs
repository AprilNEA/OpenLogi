//! Popover section categories for the action catalog.

/// Grouping for popover section headers.
///
/// Used by [`Action::category`] and rendered as a small muted label above
/// each group in the action picker.
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
    /// Short label for popover section headers (already uppercase so callers
    /// don't have to transform it).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Category::Editing => "EDITING",
            Category::Browser => "BROWSER",
            Category::Media => "MEDIA",
            Category::Mouse => "MOUSE",
            Category::Dpi => "DPI",
            Category::Scroll => "SCROLL",
            Category::Navigation => "NAVIGATION",
            Category::System => "SYSTEM",
        }
    }
}
