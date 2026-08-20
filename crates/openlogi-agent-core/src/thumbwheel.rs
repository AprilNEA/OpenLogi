//! Per-device thumb-wheel polarity consumed by the OS hook's native
//! horizontal-scroll fallback.

use std::collections::HashMap;

/// Native thumb-wheel rotation polarity per device — whether a positive
/// `WM_MOUSEHWHEEL` delta is the physical forward direction — learned from
/// HID++ `0x2150` `getThumbwheelInfo` when a capture session arms the wheel,
/// plus the selection to resolve it against.
///
/// Like [`crate::DpiCycles`], HID++ capture dispatch knows which device an
/// event arrived on, but the OS hook cannot attribute a native scroll tick to
/// a device, so it resolves against the selection — the same device whose
/// binding maps it already dispatches with. Lives *inside*
/// [`crate::hook_runtime::HookMaps`] (never behind its own lock), so the hook
/// callback reads polarity and bindings as one snapshot: the capture watcher
/// inserts learned entries under the hook-maps lock, and the orchestrator
/// republishes the selection together with the binding maps.
///
/// Entries are never pruned: one `bool` per device ever probed, and a device
/// that reconnects simply re-learns its value on the next capture session.
#[derive(Debug, Clone, Default)]
pub struct ThumbwheelDirs {
    /// Config key of the GUI-selected device (the OS hook's dispatch target).
    pub selected: Option<String>,
    /// Config key → whether a positive native horizontal-scroll delta is the
    /// physical forward direction (`0x2150` `default_dir == 1`).
    pub by_key: HashMap<String, bool>,
}

impl ThumbwheelDirs {
    /// Whether the selected device's positive native delta is physically
    /// forward, or `None` while unknown (no selection, or a wheel that was
    /// never probed — no `0x2150`, or HID++ unreachable).
    #[must_use]
    pub fn selected_positive_is_forward(&self) -> Option<bool> {
        self.by_key.get(self.selected.as_deref()?).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_needs_both_a_selection_and_a_probed_wheel() {
        let mut dirs = ThumbwheelDirs::default();
        // No selection → nothing to resolve, even with a probed entry.
        dirs.by_key.insert("a".to_owned(), true);
        assert_eq!(dirs.selected_positive_is_forward(), None);

        // A selection whose wheel was never probed stays unknown, so the hook
        // falls back to the historical mapping instead of guessing.
        dirs.selected = Some("b".to_owned());
        assert_eq!(dirs.selected_positive_is_forward(), None);

        // Selection + probed entry resolves to that device's polarity.
        dirs.selected = Some("a".to_owned());
        assert_eq!(dirs.selected_positive_is_forward(), Some(true));
        dirs.by_key.insert("a".to_owned(), false);
        assert_eq!(dirs.selected_positive_is_forward(), Some(false));
    }
}
