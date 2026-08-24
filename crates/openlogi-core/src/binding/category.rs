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
    /// Short label for popover section headers, and the i18n key it is looked
    /// up by. Sentence case: a UI layer cannot uppercase a *translated* label
    /// safely — casing rules are per-language, and GPUI has no `text-transform`
    /// to defer the decision to — so the catalog carries the cased text it
    /// wants. `DPI` is an acronym, not a shout.
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
}
