//! Actions Ring configuration vocabulary.
//!
//! The ring is host-side UI: a trigger opens an eight-position layout and the
//! agent executes the selected action. The types live beside [`Action`] because
//! they are persisted directly in `config.toml` and shared by the agent and GUI.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

use super::Action;

/// One of the eight fixed positions in an Actions Ring, clockwise from the top.
///
/// Variant names are part of the TOML schema and must remain stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ActionRingSlot {
    /// Twelve o'clock.
    Top,
    /// Between top and right.
    TopRight,
    /// Three o'clock.
    Right,
    /// Between right and bottom.
    BottomRight,
    /// Six o'clock.
    Bottom,
    /// Between bottom and left.
    BottomLeft,
    /// Nine o'clock.
    Left,
    /// Between left and top.
    TopLeft,
}

impl ActionRingSlot {
    /// All ring positions in clockwise display order.
    pub const ALL: [Self; 8] = [
        Self::Top,
        Self::TopRight,
        Self::Right,
        Self::BottomRight,
        Self::Bottom,
        Self::BottomLeft,
        Self::Left,
        Self::TopLeft,
    ];
}

/// User-selected presentation icon for an Actions Ring slot.
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
}

impl ActionRingIcon {
    /// Every icon offered by the Actions Ring editor.
    pub const ALL: [Self; 53] = [
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
    ];

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
        }
    }
}

/// Why an [`Action`] cannot be placed in an Actions Ring slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum RingActionError {
    /// Empty slots are represented by an absent map entry, not `Action::None`.
    #[error("Do Nothing is represented by an empty Actions Ring slot")]
    EmptyAction,
    /// A ring cannot recursively open itself.
    #[error("Show Actions Ring cannot be assigned inside an Actions Ring")]
    RecursiveTrigger,
}

/// An action that is valid inside an Actions Ring.
///
/// Construction and deserialization reject actions that would make the ring's
/// state ambiguous (`None`) or recursively invoke another ring
/// (`ShowActionsRing`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RingAction(Action);

impl RingAction {
    /// Validate and wrap an ordinary action for placement in a ring.
    pub fn new(action: Action) -> Result<Self, RingActionError> {
        match action {
            Action::None => Err(RingActionError::EmptyAction),
            Action::ShowActionsRing => Err(RingActionError::RecursiveTrigger),
            other => Ok(Self(other)),
        }
    }

    /// The action the agent should execute when this slot is activated.
    #[must_use]
    pub fn action(&self) -> &Action {
        &self.0
    }

    /// Consume the wrapper and return its action.
    #[must_use]
    pub fn into_action(self) -> Action {
        self.0
    }
}

impl TryFrom<Action> for RingAction {
    type Error = RingActionError;

    fn try_from(action: Action) -> Result<Self, Self::Error> {
        Self::new(action)
    }
}

impl Serialize for RingAction {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RingAction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let action = Action::deserialize(deserializer)?;
        Self::new(action).map_err(de::Error::custom)
    }
}

/// The actions displayed at the eight fixed ring positions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRingLayout {
    /// Populated ring positions. An absent key is an intentionally empty slot.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub slots: BTreeMap<ActionRingSlot, RingAction>,
    /// Optional custom presentation icon for each populated slot.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub icons: BTreeMap<ActionRingSlot, ActionRingIcon>,
}

impl Default for ActionRingLayout {
    fn default() -> Self {
        use ActionRingSlot as Slot;

        let actions = [
            (Slot::Top, Action::Cut),
            (Slot::TopRight, Action::Copy),
            (Slot::Right, Action::Paste),
            (Slot::BottomRight, Action::BrowserForward),
            (Slot::Bottom, Action::PlayPause),
            (Slot::BottomLeft, Action::BrowserBack),
            (Slot::Left, Action::Undo),
            (Slot::TopLeft, Action::Redo),
        ];
        let slots = actions
            .into_iter()
            .map(|(slot, action)| (slot, RingAction(action)))
            .collect();
        Self {
            slots,
            icons: BTreeMap::new(),
        }
    }
}

/// Per-device Actions Ring settings and application-specific layouts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRingConfig {
    /// Whether `ShowActionsRing` opens this device's ring.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Whether ring hover and activation transitions play device haptics.
    #[serde(default = "default_true")]
    pub haptics: bool,
    /// Layout used when the foreground application has no override.
    #[serde(default)]
    pub default: ActionRingLayout,
    /// Complete layout overrides keyed by foreground application identifier.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_app: BTreeMap<String, ActionRingLayout>,
}

impl Default for ActionRingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            haptics: true,
            default: ActionRingLayout::default(),
            per_app: BTreeMap::new(),
        }
    }
}

impl ActionRingConfig {
    /// Whether this value is exactly the implicit default and can be omitted
    /// from `config.toml`.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    /// Resolve the complete layout for the foreground application.
    #[must_use]
    pub fn effective_layout(&self, app_id: Option<&str>) -> ActionRingLayout {
        app_id
            .and_then(|app| self.per_app.get(app))
            .cloned()
            .unwrap_or_else(|| self.default.clone())
    }
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layout_populates_every_position() {
        let layout = ActionRingLayout::default();
        assert_eq!(layout.slots.len(), ActionRingSlot::ALL.len());
        assert!(layout.icons.is_empty());
        assert!(
            ActionRingSlot::ALL
                .iter()
                .all(|slot| layout.slots.contains_key(slot))
        );
    }

    #[test]
    fn invalid_ring_actions_are_rejected() {
        assert_eq!(
            RingAction::new(Action::None),
            Err(RingActionError::EmptyAction)
        );
        assert_eq!(
            RingAction::new(Action::ShowActionsRing),
            Err(RingActionError::RecursiveTrigger)
        );
    }

    #[test]
    fn ring_action_serializes_like_the_wrapped_action() {
        #[derive(Serialize)]
        struct Wrapper {
            action: RingAction,
        }

        let action = RingAction::new(Action::Copy).unwrap_or_else(|error| panic!("{error}"));
        let encoded = toml::to_string(&Wrapper { action })
            .unwrap_or_else(|error| panic!("could not serialize ring action: {error}"));
        assert_eq!(encoded, "action = \"Copy\"\n");
    }

    #[test]
    fn custom_icons_roundtrip_without_changing_slot_actions() {
        let mut layout = ActionRingLayout::default();
        layout
            .icons
            .insert(ActionRingSlot::Top, ActionRingIcon::Keyboard);
        let encoded = toml::to_string(&layout)
            .unwrap_or_else(|error| panic!("could not serialize ring layout: {error}"));
        let decoded = toml::from_str::<ActionRingLayout>(&encoded)
            .unwrap_or_else(|error| panic!("could not deserialize ring layout: {error}"));
        assert_eq!(decoded, layout);
        assert_eq!(decoded.slots[&ActionRingSlot::Top].action(), &Action::Cut);
    }

    #[test]
    fn recursive_action_fails_deserialization() {
        let parsed = toml::from_str::<RingAction>("\"ShowActionsRing\"");
        assert!(parsed.is_err());
    }

    #[test]
    fn app_layout_replaces_the_default_layout() {
        let mut config = ActionRingConfig::default();
        let safari = ActionRingLayout {
            slots: BTreeMap::from([(
                ActionRingSlot::Top,
                RingAction::new(Action::NewTab).unwrap_or_else(|error| panic!("{error}")),
            )]),
            icons: BTreeMap::new(),
        };
        config
            .per_app
            .insert("com.apple.Safari".to_string(), safari.clone());

        assert_eq!(config.effective_layout(Some("com.apple.Safari")), safari);
        assert_eq!(config.effective_layout(Some("other")), config.default);
    }
}
