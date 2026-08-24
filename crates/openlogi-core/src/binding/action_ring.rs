//! Actions Ring configuration vocabulary.
//!
//! The ring is host-side UI: a trigger opens an eight-position layout and the
//! agent executes the selected action. The types live beside [`Action`] because
//! they are persisted directly in `config.toml` and shared by the agent and GUI.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use nutype::nutype;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::Action;

mod icon;

pub use icon::ActionRingIcon;

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

    /// Unit vector from the ring's centre to this slot, with positive Y
    /// pointing **down** — the screen convention both GPUI frontends draw in,
    /// not the mathematical one.
    #[must_use]
    pub fn unit_offset(self) -> (f32, f32) {
        let diagonal = std::f32::consts::FRAC_1_SQRT_2;
        match self {
            Self::Top => (0.0, -1.0),
            Self::TopRight => (diagonal, -diagonal),
            Self::Right => (1.0, 0.0),
            Self::BottomRight => (diagonal, diagonal),
            Self::Bottom => (0.0, 1.0),
            Self::BottomLeft => (-diagonal, diagonal),
            Self::Left => (-1.0, 0.0),
            Self::TopLeft => (-diagonal, -diagonal),
        }
    }

    /// Top-left corner at which to place this slot's `slot_size` box, on a
    /// square `canvas` whose ring has the given `radius`. All in the caller's
    /// own units; the live overlay and the settings preview differ only in
    /// what they pass.
    #[must_use]
    pub fn placement(self, canvas: f32, radius: f32, slot_size: f32) -> (f32, f32) {
        let (x, y) = self.unit_offset();
        (
            canvas / 2.0 + x * radius - slot_size / 2.0,
            canvas / 2.0 + y * radius - slot_size / 2.0,
        )
    }

    /// Stable display index matching [`Self::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Top => 0,
            Self::TopRight => 1,
            Self::Right => 2,
            Self::BottomRight => 3,
            Self::Bottom => 4,
            Self::BottomLeft => 5,
            Self::Left => 6,
            Self::TopLeft => 7,
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
#[nutype(
    validate(with = validate_ring_action, error = RingActionError),
    derive(Clone, Debug, PartialEq, Eq, Hash, AsRef, TryFrom, Into, Serialize, Deserialize),
)]
pub struct RingAction(Action);

impl RingAction {
    /// Validate and wrap an ordinary action for placement in a ring.
    pub fn new(action: Action) -> Result<Self, RingActionError> {
        Self::try_new(action)
    }

    /// The action the agent should execute when this slot is activated.
    #[must_use]
    pub fn action(&self) -> &Action {
        self.as_ref()
    }

    /// Consume the wrapper and return its action.
    #[must_use]
    pub fn into_action(self) -> Action {
        self.into_inner()
    }
}

fn validate_ring_action(action: &Action) -> Result<(), RingActionError> {
    match action {
        Action::None => Err(RingActionError::EmptyAction),
        Action::ShowActionsRing => Err(RingActionError::RecursiveTrigger),
        _ => Ok(()),
    }
}

/// One populated Actions Ring slot.
///
/// Keeping the action and optional presentation icon in one value makes an
/// orphan icon impossible: clearing a slot removes the complete entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRingEntry {
    action: RingAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<ActionRingIcon>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

impl ActionRingEntry {
    /// Create a slot with its action-derived icon and label.
    #[must_use]
    pub const fn new(action: RingAction) -> Self {
        Self {
            action,
            icon: None,
            label: None,
        }
    }

    /// Executable action for this slot.
    #[must_use]
    pub fn action(&self) -> &Action {
        self.action.action()
    }

    /// User-selected icon, or `None` to derive it from [`Self::action`].
    #[must_use]
    pub const fn custom_icon(&self) -> Option<ActionRingIcon> {
        self.icon
    }

    /// User-provided display label, or `None` to derive one from
    /// [`Self::action`]. Free text: the overlay's localization pass returns
    /// unknown keys verbatim, so user labels render as written.
    #[must_use]
    pub fn custom_label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Consume the entry into its executable action and presentation
    /// overrides (icon, label).
    #[must_use]
    pub fn into_parts(self) -> (Action, Option<ActionRingIcon>, Option<String>) {
        (self.action.into_action(), self.icon, self.label)
    }

    fn replace_action(&mut self, action: RingAction) {
        self.action = action;
    }

    fn set_icon(&mut self, icon: Option<ActionRingIcon>) {
        self.icon = icon;
    }

    fn set_label(&mut self, label: Option<String>) {
        self.label = label;
    }
}

/// The actions displayed at the eight fixed ring positions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRingLayout {
    /// Populated ring positions. An absent key is an intentionally empty slot.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub slots: BTreeMap<ActionRingSlot, ActionRingEntry>,
}

impl ActionRingLayout {
    /// Replace or clear a slot while preserving its custom icon when replaced.
    pub fn set_action(&mut self, slot: ActionRingSlot, action: Option<RingAction>) {
        match (self.slots.entry(slot), action) {
            (Entry::Occupied(mut entry), Some(action)) => entry.get_mut().replace_action(action),
            (Entry::Vacant(entry), Some(action)) => {
                entry.insert(ActionRingEntry::new(action));
            }
            (Entry::Occupied(entry), None) => {
                entry.remove();
            }
            (Entry::Vacant(_), None) => {}
        }
    }

    /// Set a custom icon for a populated slot. Empty slots remain empty.
    pub fn set_icon(&mut self, slot: ActionRingSlot, icon: Option<ActionRingIcon>) {
        if let Some(entry) = self.slots.get_mut(&slot) {
            entry.set_icon(icon);
        }
    }

    /// Set a custom label for a populated slot. Empty slots remain empty.
    pub fn set_label(&mut self, slot: ActionRingSlot, label: Option<String>) {
        if let Some(entry) = self.slots.get_mut(&slot) {
            entry.set_label(label);
        }
    }
}

impl Default for ActionRingLayout {
    #[expect(
        clippy::expect_used,
        reason = "the built-in ring actions are statically known to satisfy RingAction's invariant"
    )]
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
            .map(|(slot, action)| {
                (
                    slot,
                    ActionRingEntry::new(
                        RingAction::new(action).expect("default ring actions must be valid"),
                    ),
                )
            })
            .collect();
        Self { slots }
    }
}

/// Per-device Actions Ring settings and application-specific layouts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

        let action = RingAction::new(Action::Copy).expect("copy must be a valid ring action");
        let encoded =
            toml::to_string(&Wrapper { action }).expect("could not serialize ring action");
        assert_eq!(encoded, "action = \"Copy\"\n");
    }

    #[test]
    fn custom_labels_roundtrip_and_survive_action_replacement() {
        let mut layout: ActionRingLayout = toml::from_str(
            r#"
            [slots]
            Top = { action = "Copy", label = "Copy Invoice" }
            "#,
        )
        .expect("could not deserialize labelled layout");
        assert_eq!(
            layout.slots[&ActionRingSlot::Top].custom_label(),
            Some("Copy Invoice")
        );

        let encoded = toml::to_string(&layout).expect("could not serialize labelled layout");
        let decoded = toml::from_str::<ActionRingLayout>(&encoded)
            .expect("could not deserialize labelled layout");
        assert_eq!(decoded, layout);

        // Like icons, a label sticks to its slot when the action is replaced.
        layout.set_action(
            ActionRingSlot::Top,
            Some(RingAction::new(Action::Paste).expect("paste must be a valid ring action")),
        );
        assert_eq!(
            layout.slots[&ActionRingSlot::Top].custom_label(),
            Some("Copy Invoice")
        );
    }

    #[test]
    fn unlabelled_entries_serialize_without_a_label_key() {
        let layout = ActionRingLayout::default();
        let encoded = toml::to_string(&layout).expect("could not serialize ring layout");
        assert!(!encoded.contains("label"));
    }

    #[test]
    fn clearing_a_slot_cannot_leave_an_orphan_icon() {
        let mut layout = ActionRingLayout::default();
        layout.set_icon(ActionRingSlot::Top, Some(ActionRingIcon::Keyboard));
        layout.set_action(ActionRingSlot::Top, None);
        assert!(!layout.slots.contains_key(&ActionRingSlot::Top));
    }

    #[test]
    fn custom_icons_roundtrip_without_changing_slot_actions() {
        let mut layout = ActionRingLayout::default();
        layout.set_icon(ActionRingSlot::Top, Some(ActionRingIcon::Keyboard));
        let encoded = toml::to_string(&layout).expect("could not serialize ring layout");
        let decoded = toml::from_str::<ActionRingLayout>(&encoded)
            .expect("could not deserialize ring layout");
        assert_eq!(decoded, layout);
        assert_eq!(decoded.slots[&ActionRingSlot::Top].action(), &Action::Cut);
        assert_eq!(
            decoded.slots[&ActionRingSlot::Top].custom_icon(),
            Some(ActionRingIcon::Keyboard)
        );
    }

    #[test]
    fn documented_inline_slots_deserialize() {
        let layout = toml::from_str::<ActionRingLayout>(
            r#"
[slots]
Top = { action = "Copy", icon = "Keyboard" }
Bottom = { action = { CustomShortcut = "Cmd+Shift+P" } }
"#,
        )
        .expect("documented ring layout failed");
        assert_eq!(layout.slots[&ActionRingSlot::Top].action(), &Action::Copy);
        assert_eq!(
            layout.slots[&ActionRingSlot::Top].custom_icon(),
            Some(ActionRingIcon::Keyboard)
        );
        assert!(matches!(
            layout.slots[&ActionRingSlot::Bottom].action(),
            Action::CustomShortcut(_)
        ));
    }

    #[test]
    fn recursive_action_fails_deserialization() {
        // A bare `"ShowActionsRing"` is not a TOML document, so the slot has to
        // be deserialized the way config.toml stores it — otherwise the parse
        // fails on syntax and never reaches the recursion guard.
        let error = match toml::from_str::<ActionRingEntry>("action = \"ShowActionsRing\"") {
            Ok(entry) => panic!("a ring slot must not recursively open the ring, got {entry:?}"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains(&RingActionError::RecursiveTrigger.to_string()),
            "expected the recursion guard to reject the slot, got: {error}"
        );
    }

    #[test]
    fn app_layout_replaces_the_default_layout() {
        let mut config = ActionRingConfig::default();
        let safari = ActionRingLayout {
            slots: BTreeMap::from([(
                ActionRingSlot::Top,
                ActionRingEntry::new(
                    RingAction::new(Action::NewTab).expect("new tab must be a valid ring action"),
                ),
            )]),
        };
        config
            .per_app
            .insert("com.apple.Safari".to_string(), safari.clone());

        assert_eq!(config.effective_layout(Some("com.apple.Safari")), safari);
        assert_eq!(config.effective_layout(Some("other")), config.default);
    }
}
