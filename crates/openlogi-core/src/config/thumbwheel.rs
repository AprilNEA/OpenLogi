//! Per-application thumb-wheel rules, the `[thumbwheel_apps]` table.

use serde::{Deserialize, Serialize};

use crate::scroll::ScrollDelivery;

/// What one application's rule changes about thumb-wheel output. The table is
/// keyed by application selector, matched the way per-app button profiles are
/// (see [`Config::scroll_delivery`](super::Config::scroll_delivery)).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThumbwheelAppRule {
    /// How horizontal scrolling reaches the application. Absent keeps the
    /// native horizontal wheel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<ScrollDelivery>,
}
