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
    /// Path of this glyph's embedded SVG, as the GPUI frontends' asset source
    /// serves it. The catalogue of shipped glyphs is a property of the icon
    /// set, so it lives with the variants rather than in either frontend —
    /// `openlogi-ui` owns the bytes and tests that every variant resolves.
    #[must_use]
    pub const fn asset_path(self) -> &'static str {
        match self {
            Self::Pointer => "action-icons/mouse-pointer-click.svg",
            Self::Mouse => "action-icons/mouse.svg",
            Self::Copy => "action-icons/copy.svg",
            Self::Paste => "action-icons/clipboard-paste.svg",
            Self::Cut => "action-icons/scissors.svg",
            Self::Search => "action-icons/search.svg",
            Self::Save => "action-icons/save.svg",
            Self::Keyboard => "action-icons/keyboard.svg",
            Self::Applications => "action-icons/grid-3x3.svg",
            Self::Grid => "action-icons/layout-grid.svg",
            Self::Layers => "action-icons/layers.svg",
            Self::Monitor => "action-icons/monitor.svg",
            Self::Lock => "action-icons/lock.svg",
            Self::Camera => "action-icons/camera.svg",
            Self::Play => "action-icons/play.svg",
            Self::Volume => "action-icons/volume-2.svg",
            Self::Gauge => "action-icons/gauge.svg",
            Self::Refresh => "action-icons/refresh-cw.svg",
            Self::ArrowUp => "action-icons/chevrons-up.svg",
            Self::ArrowDown => "action-icons/chevrons-down.svg",
            Self::ArrowLeft => "action-icons/arrow-left.svg",
            Self::ArrowRight => "action-icons/arrow-right.svg",
            Self::Undo => "action-icons/undo-2.svg",
            Self::Redo => "action-icons/redo-2.svg",
            Self::SelectAll => "action-icons/list-checks.svg",
            Self::MouseBack => "action-icons/circle-arrow-left.svg",
            Self::MouseForward => "action-icons/circle-arrow-right.svg",
            Self::NewTab => "action-icons/square-plus.svg",
            Self::CloseTab => "action-icons/square-x.svg",
            Self::ReopenTab => "action-icons/rotate-ccw.svg",
            Self::NextTab => "action-icons/chevron-right.svg",
            Self::PreviousTab => "action-icons/chevron-left.svg",
            Self::Reload => "action-icons/rotate-cw.svg",
            Self::PreviousDesktop => "action-icons/square-arrow-left.svg",
            Self::NextDesktop => "action-icons/square-arrow-right.svg",
            Self::PreviousTrack => "action-icons/skip-back.svg",
            Self::NextTrack => "action-icons/skip-forward.svg",
            Self::VolumeDown => "action-icons/volume-1.svg",
            Self::Mute => "action-icons/volume-x.svg",
            Self::ScrollLeft => "action-icons/chevrons-left.svg",
            Self::ScrollRight => "action-icons/chevrons-right.svg",
            Self::Folder => "action-icons/folder.svg",
            Self::File => "action-icons/file.svg",
            Self::Globe => "action-icons/globe.svg",
            Self::Terminal => "action-icons/square-terminal.svg",
            Self::Settings => "action-icons/settings.svg",
            Self::Star => "action-icons/star.svg",
            Self::Heart => "action-icons/heart.svg",
            Self::Calendar => "action-icons/calendar.svg",
            Self::Bell => "action-icons/bell.svg",
            Self::User => "action-icons/user.svg",
            Self::Palette => "action-icons/palette.svg",
            Self::Book => "action-icons/book-open.svg",
            Self::Ban => "action-icons/ban.svg",
        }
    }

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

    /// Stable catalog key used as this icon's accessible label.
    #[must_use]
    pub fn translation_key(self) -> &'static str {
        match self {
            Self::Pointer => "actions.left_click",
            Self::Mouse => "actions.middle_click",
            Self::Copy => "common.copy",
            Self::Paste => "common.paste",
            Self::Cut => "common.cut",
            Self::Search => "actions.find",
            Self::Save => "common.save",
            Self::Keyboard => "action_ring.custom_shortcut",
            Self::Applications => "action_ring.open_application_or_folder",
            Self::Grid => "action_ring.actions_ring",
            Self::Layers => "actions.app_expose",
            Self::Monitor => "actions.show_desktop",
            Self::Lock => "actions.lock_screen",
            Self::Camera => "actions.screenshot",
            Self::Play => "actions.play_pause",
            Self::Volume => "actions.volume_up",
            Self::Gauge => "pointer.cycle_dpi_presets",
            Self::Refresh => "pointer.toggle_smartshift",
            Self::ArrowUp => "actions.scroll_up",
            Self::ArrowDown => "actions.scroll_down",
            Self::ArrowLeft | Self::ScrollLeft => "actions.scroll_left",
            Self::ArrowRight | Self::ScrollRight => "actions.scroll_right",
            Self::Undo => "common.undo",
            Self::Redo => "common.redo",
            Self::SelectAll => "common.select_all",
            Self::MouseBack => "actions.back_button_4",
            Self::MouseForward => "actions.forward_button_5",
            Self::NewTab => "actions.new_tab",
            Self::CloseTab => "actions.close_tab",
            Self::ReopenTab => "actions.reopen_tab",
            Self::NextTab => "actions.next_tab",
            Self::PreviousTab => "actions.previous_tab",
            Self::Reload => "actions.reload_page",
            Self::PreviousDesktop => "actions.previous_desktop",
            Self::NextDesktop => "actions.next_desktop",
            Self::PreviousTrack => "actions.previous_track",
            Self::NextTrack => "actions.next_track",
            Self::VolumeDown => "actions.volume_down",
            Self::Mute => "actions.mute",
            Self::Folder => "action_ring.folder",
            Self::File => "action_ring.file",
            Self::Globe => "action_ring.globe",
            Self::Terminal => "action_ring.terminal",
            Self::Settings => "app.settings",
            Self::Star => "action_ring.star",
            Self::Heart => "action_ring.heart",
            Self::Calendar => "action_ring.calendar",
            Self::Bell => "action_ring.bell",
            Self::User => "action_ring.user",
            Self::Palette => "action_ring.palette",
            Self::Book => "action_ring.book",
            Self::Ban => "pointer.do_nothing",
        }
    }
}

/// Builds [`ActionRingIcon::for_action`] from
/// [`for_each_unit_action!`](super::super::action::for_each_unit_action)'s
/// rows, splicing in the hand-written arms for payload-carrying variants so
/// the generated `match` still covers every [`Action`] variant exhaustively.
macro_rules! derive_action_icon {
    ( $( $variant:ident $label:literal $translation_key:literal $category:ident $icon:ident $( $tag:ident )? ),* $(,)? ) => {
        impl ActionRingIcon {
            /// Default icon for an executable action.
            #[must_use]
            pub fn for_action(action: &Action) -> Self {
                match action {
                    $( Action::$variant => Self::$icon, )*
                    Action::SetDpiPreset(_) => Self::Gauge,
                    Action::CustomShortcut(_)
                    | Action::TypeText(_)
                    | Action::Workflow(_)
                    | Action::HoldShortcut(_) => Self::Keyboard,
                    Action::RunAppleScript(_) | Action::RunShellCommand(_) => Self::Terminal,
                    Action::OpenApplication(_) => Self::Applications,
                }
            }
        }
    };
}

super::super::action::for_each_unit_action!(derive_action_icon);
