//! Flow — edge-triggered multi-host switching, configured per device.
//!
//! Pushing the cursor into a mapped screen edge tells the agent to move the
//! pointing device (and every device following it) to another paired host via
//! HID++ `ChangeHost`; the machine on the other side runs its own OpenLogi
//! and brings them back from its opposite edge. Fully local — no networking.
//!
//! Two roles, two fields on [`DeviceConfig`](super::DeviceConfig): a pointing
//! device carries a [`FlowConfig`] (its cursor drives the switching), and any
//! device may carry a [`FlowFollow`] (whether it tracks a Flow pointer's host
//! switches).

use serde::{Deserialize, Serialize};

/// The `[devices."…".flow]` table — the pointer role.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowConfig {
    /// Master toggle. Off by default: an edge that moves the mouse to another
    /// computer must be something the user asked for.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub enabled: bool,
    /// Which host each screen side switches to.
    #[serde(default, skip_serializing_if = "FlowPlacements::is_empty")]
    pub placements: FlowPlacements,
    /// What arms an edge: plain contact, or contact while Ctrl is held.
    #[serde(default, skip_serializing_if = "FlowTriggerMode::is_default")]
    pub trigger: FlowTriggerMode,
}

impl FlowConfig {
    /// `skip_serializing_if` helper: true when nothing diverges from the
    /// default, so untouched devices keep `config.toml` clean.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// One optional host per screen side — the arrangement the Flow tab's
/// drag-and-drop cards edit. A side maps to its edge plus both adjacent
/// corners (left ⇒ top-left + left + bottom-left). One slot per side makes a
/// duplicate side unrepresentable; hosts are the device's zero-based
/// `ChangeHost` slots.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowPlacements {
    /// Host reached by pushing through the left edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<u8>,
    /// Host reached by pushing through the right edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<u8>,
    /// Host reached by pushing through the top edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<u8>,
    /// Host reached by pushing through the bottom edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom: Option<u8>,
}

impl FlowPlacements {
    /// Whether no side is mapped.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }

    /// How many sides are mapped.
    #[must_use]
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    /// The host mapped on `side`, if any.
    #[must_use]
    pub const fn get(&self, side: FlowSide) -> Option<u8> {
        match side {
            FlowSide::Left => self.left,
            FlowSide::Right => self.right,
            FlowSide::Top => self.top,
            FlowSide::Bottom => self.bottom,
        }
    }

    /// Map `side` to `host`, or clear it with `None`.
    pub fn set(&mut self, side: FlowSide, host: Option<u8>) {
        *self.slot_mut(side) = host;
    }

    /// Every mapped `(side, host)` pair, in [`FlowSide::ALL`] order.
    pub fn iter(&self) -> impl Iterator<Item = (FlowSide, u8)> + '_ {
        FlowSide::ALL
            .iter()
            .filter_map(|&side| self.get(side).map(|host| (side, host)))
    }

    const fn slot_mut(&mut self, side: FlowSide) -> &mut Option<u8> {
        match side {
            FlowSide::Left => &mut self.left,
            FlowSide::Right => &mut self.right,
            FlowSide::Top => &mut self.top,
            FlowSide::Bottom => &mut self.bottom,
        }
    }
}

/// A display side a computer card can snap to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowSide {
    /// The left screen edge.
    Left,
    /// The right screen edge.
    Right,
    /// The top screen edge.
    Top,
    /// The bottom screen edge.
    Bottom,
}

impl FlowSide {
    /// Every side, in declaration order — iteration order for the GUI's slots
    /// and for [`FlowPlacements::iter`].
    pub const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Top, Self::Bottom];
}

/// What arms a mapped edge.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowTriggerMode {
    /// Pushing the cursor against the edge switches.
    #[default]
    Edge,
    /// The edge only switches while a Ctrl key is held — guards against
    /// accidental grazes.
    CtrlEdge,
}

impl FlowTriggerMode {
    /// `skip_serializing_if` helper.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The follower role: whether this device tracks a Flow pointer's host
/// switches (`flow_follow` on the device table).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowFollow {
    /// Follow the Flow-enabled pointing device without setup — the default
    /// that makes a keyboard jump with the mouse out of the box. Applies to
    /// keyboards only; other device kinds must opt in with [`Self::Device`].
    #[default]
    Auto,
    /// Never follow.
    Off,
    /// Follow the pointer with this physical config key specifically.
    Device(String),
}

impl FlowFollow {
    /// `skip_serializing_if` helper.
    #[must_use]
    pub fn is_auto(&self) -> bool {
        *self == Self::Auto
    }
}
