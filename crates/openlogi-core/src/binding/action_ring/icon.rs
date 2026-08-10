use serde::{Deserialize, Serialize};

use super::super::Action;

/// Presentation icon for an Actions Ring slot.
///
/// Variant names are persisted in TOML and declaration order is part of the
/// agent IPC wire format, so variants are append-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ActionRingIcon {
    /// Pointer click glyph.
    Pointer,
    /// Physical mouse glyph.
    Mouse,
    /// Copy glyph.
    Copy,
    /// Clipboard/paste glyph.
    Paste,
    /// Scissors/cut glyph.
    Cut,
    /// Search glyph.
    Search,
    /// Save glyph.
    Save,
    /// Keyboard glyph.
    Keyboard,
    /// Application grid glyph.
    Applications,
    /// Actions grid glyph.
    Grid,
    /// Layer stack glyph.
    Layers,
    /// Display glyph.
    Monitor,
    /// Lock glyph.
    Lock,
    /// Camera glyph.
    Camera,
    /// Playback glyph.
    Play,
    /// Audio glyph.
    Volume,
    /// Gauge glyph.
    Gauge,
    /// Refresh glyph.
    Refresh,
    /// Up arrow glyph.
    ArrowUp,
    /// Down arrow glyph.
    ArrowDown,
    /// Left arrow glyph.
    ArrowLeft,
    /// Right arrow glyph.
    ArrowRight,
    /// Undo glyph.
    Undo,
    /// Redo glyph.
    Redo,
    /// Selection checklist glyph.
    SelectAll,
    /// Circular back glyph.
    MouseBack,
    /// Circular forward glyph.
    MouseForward,
    /// New-tab glyph.
    NewTab,
    /// Close-tab glyph.
    CloseTab,
    /// Reopen-tab glyph.
    ReopenTab,
    /// Next-tab glyph.
    NextTab,
    /// Previous-tab glyph.
    PreviousTab,
    /// Reload glyph.
    Reload,
    /// Previous-desktop glyph.
    PreviousDesktop,
    /// Next-desktop glyph.
    NextDesktop,
    /// Previous-track glyph.
    PreviousTrack,
    /// Next-track glyph.
    NextTrack,
    /// Lower-volume glyph.
    VolumeDown,
    /// Muted-volume glyph.
    Mute,
    /// Horizontal scroll-left glyph.
    ScrollLeft,
    /// Horizontal scroll-right glyph.
    ScrollRight,
    /// Folder glyph.
    Folder,
    /// File glyph.
    File,
    /// Globe glyph.
    Globe,
    /// Terminal glyph.
    Terminal,
    /// Settings glyph.
    Settings,
    /// Star glyph.
    Star,
    /// Heart glyph.
    Heart,
    /// Calendar glyph.
    Calendar,
    /// Notification bell glyph.
    Bell,
    /// User glyph.
    User,
    /// Color palette glyph.
    Palette,
    /// Open book glyph.
    Book,
    /// Prohibited action glyph.
    Ban,
}

impl ActionRingIcon {
    /// Every icon offered by the Actions Ring editor.
    pub const ALL: [Self; 54] = [
        Self::Pointer,
        Self::Mouse,
        Self::Copy,
        Self::Paste,
        Self::Cut,
        Self::Search,
        Self::Save,
        Self::Keyboard,
        Self::Applications,
        Self::Grid,
        Self::Layers,
        Self::Monitor,
        Self::Lock,
        Self::Camera,
        Self::Play,
        Self::Volume,
        Self::Gauge,
        Self::Refresh,
        Self::ArrowUp,
        Self::ArrowDown,
        Self::ArrowLeft,
        Self::ArrowRight,
        Self::Undo,
        Self::Redo,
        Self::SelectAll,
        Self::MouseBack,
        Self::MouseForward,
        Self::NewTab,
        Self::CloseTab,
        Self::ReopenTab,
        Self::NextTab,
        Self::PreviousTab,
        Self::Reload,
        Self::PreviousDesktop,
        Self::NextDesktop,
        Self::PreviousTrack,
        Self::NextTrack,
        Self::VolumeDown,
        Self::Mute,
        Self::ScrollLeft,
        Self::ScrollRight,
        Self::Folder,
        Self::File,
        Self::Globe,
        Self::Terminal,
        Self::Settings,
        Self::Star,
        Self::Heart,
        Self::Calendar,
        Self::Bell,
        Self::User,
        Self::Palette,
        Self::Book,
        Self::Ban,
    ];

    /// Default icon for an executable action.
    #[must_use]
    pub fn for_action(action: &Action) -> Self {
        match action {
            Action::None => Self::Ban,
            Action::LeftClick | Action::RightClick => Self::Pointer,
            Action::MiddleClick => Self::Mouse,
            Action::MouseBack => Self::MouseBack,
            Action::MouseForward => Self::MouseForward,
            Action::Copy => Self::Copy,
            Action::Paste => Self::Paste,
            Action::Cut => Self::Cut,
            Action::Undo => Self::Undo,
            Action::Redo => Self::Redo,
            Action::SelectAll => Self::SelectAll,
            Action::Find => Self::Search,
            Action::Save => Self::Save,
            Action::BrowserBack => Self::ArrowLeft,
            Action::BrowserForward => Self::ArrowRight,
            Action::NewTab => Self::NewTab,
            Action::CloseTab => Self::CloseTab,
            Action::ReopenTab => Self::ReopenTab,
            Action::NextTab => Self::NextTab,
            Action::PrevTab => Self::PreviousTab,
            Action::ReloadPage => Self::Reload,
            Action::MissionControl | Action::ShowActionsRing => Self::Grid,
            Action::AppExpose => Self::Layers,
            Action::PreviousDesktop => Self::PreviousDesktop,
            Action::NextDesktop => Self::NextDesktop,
            Action::ShowDesktop | Action::Sleep => Self::Monitor,
            Action::LaunchpadShow | Action::OpenApplication(_) => Self::Applications,
            Action::LockScreen => Self::Lock,
            Action::Screenshot | Action::CaptureRegion => Self::Camera,
            Action::PlayPause => Self::Play,
            Action::NextTrack => Self::NextTrack,
            Action::PrevTrack => Self::PreviousTrack,
            Action::VolumeUp => Self::Volume,
            Action::VolumeDown => Self::VolumeDown,
            Action::MuteVolume => Self::Mute,
            Action::CycleDpiPresets | Action::SetDpiPreset(_) => Self::Gauge,
            Action::ToggleSmartShift => Self::Refresh,
            Action::ScrollUp => Self::ArrowUp,
            Action::ScrollDown => Self::ArrowDown,
            Action::HorizontalScrollLeft => Self::ScrollLeft,
            Action::HorizontalScrollRight => Self::ScrollRight,
            Action::CustomShortcut(_) | Action::TypeText(_) | Action::Workflow(_) => Self::Keyboard,
            Action::RunAppleScript(_) | Action::RunShellCommand(_) => Self::Terminal,
        }
    }

    /// Existing localization key used as this icon's accessible label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pointer => "Left Click",
            Self::Mouse => "Middle Click",
            Self::Copy => "Copy",
            Self::Paste => "Paste",
            Self::Cut => "Cut",
            Self::Search => "Find",
            Self::Save => "Save",
            Self::Keyboard => "Custom shortcut",
            Self::Applications => "Open application or folder",
            Self::Grid => "Actions Ring",
            Self::Layers => "App Exposé",
            Self::Monitor => "Show Desktop",
            Self::Lock => "Lock Screen",
            Self::Camera => "Screenshot",
            Self::Play => "Play / Pause",
            Self::Volume => "Volume Up",
            Self::Gauge => "Cycle DPI Presets",
            Self::Refresh => "Toggle SmartShift",
            Self::ArrowUp => "Scroll Up",
            Self::ArrowDown => "Scroll Down",
            Self::ArrowLeft | Self::ScrollLeft => "Scroll Left",
            Self::ArrowRight | Self::ScrollRight => "Scroll Right",
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::SelectAll => "Select All",
            Self::MouseBack => "Back (Button 4)",
            Self::MouseForward => "Forward (Button 5)",
            Self::NewTab => "New Tab",
            Self::CloseTab => "Close Tab",
            Self::ReopenTab => "Reopen Tab",
            Self::NextTab => "Next Tab",
            Self::PreviousTab => "Previous Tab",
            Self::Reload => "Reload Page",
            Self::PreviousDesktop => "Previous Desktop",
            Self::NextDesktop => "Next Desktop",
            Self::PreviousTrack => "Previous Track",
            Self::NextTrack => "Next Track",
            Self::VolumeDown => "Volume Down",
            Self::Mute => "Mute",
            Self::Folder => "Folder",
            Self::File => "File",
            Self::Globe => "Globe",
            Self::Terminal => "Terminal",
            Self::Settings => "Settings",
            Self::Star => "Star",
            Self::Heart => "Heart",
            Self::Calendar => "Calendar",
            Self::Bell => "Bell",
            Self::User => "User",
            Self::Palette => "Palette",
            Self::Book => "Book",
            Self::Ban => "Do Nothing",
        }
    }
}
