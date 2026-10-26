//! Keyboard modifiers: their bit layout and the names a binding spells them
//! with.
//!
//! [`Modifiers`] is the set a [`super::KeyCombo`] carries beside its key.

bitflags::bitflags! {
    /// A set of keyboard modifiers, possibly empty.
    ///
    /// Crosses the agent↔GUI IPC as one byte — changes to the layout require
    /// a `PROTOCOL_VERSION` bump (guarded by
    /// `openlogi-ipc/tests/wire_format.rs`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct Modifiers: u8 {
        /// Command, the cross-platform primary modifier: Command on macOS,
        /// Control on Linux and Windows.
        const COMMAND = 1 << 0;
        /// Shift.
        const SHIFT = 1 << 1;
        /// Control.
        const CONTROL = 1 << 2;
        /// Option on macOS, Alt elsewhere.
        const OPTION = 1 << 3;
        /// The logo key, written `Super`, `Win`, or `Meta`: Command on macOS,
        /// the Windows key, `KEY_LEFTMETA` on Linux. Unlike
        /// [`Self::COMMAND`], it never becomes Control.
        const SUPER = 1 << 4;
    }
}

impl Modifiers {
    /// The modifier a binding token names, ignoring case, or `None` for any
    /// other token.
    pub(super) fn from_token(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "cmd" | "command" => Some(Self::COMMAND),
            "super" | "win" | "meta" => Some(Self::SUPER),
            "shift" => Some(Self::SHIFT),
            "ctrl" | "control" => Some(Self::CONTROL),
            "alt" | "option" => Some(Self::OPTION),
            _ => None,
        }
    }

    /// Canonical user-facing label, or `None` for the empty set.
    pub(super) fn rendered_label(self) -> Option<String> {
        let mut parts = Vec::new();
        if self.contains(Self::COMMAND) {
            parts.push("Cmd");
        }
        if self.contains(Self::SUPER) {
            parts.push("Super");
        }
        if self.contains(Self::CONTROL) {
            parts.push("Ctrl");
        }
        if self.contains(Self::OPTION) {
            parts.push("Alt");
        }
        if self.contains(Self::SHIFT) {
            parts.push("Shift");
        }
        (!parts.is_empty()).then(|| parts.join("+"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_name_their_modifier_in_any_case() {
        assert_eq!(Modifiers::from_token("CMD"), Some(Modifiers::COMMAND));
        assert_eq!(Modifiers::from_token("win"), Some(Modifiers::SUPER));
        assert_eq!(Modifiers::from_token("Option"), Some(Modifiers::OPTION));
        assert_eq!(Modifiers::from_token("A"), None);
    }

    #[test]
    fn label_follows_canonical_order_and_is_none_when_empty() {
        let set = Modifiers::SHIFT | Modifiers::CONTROL | Modifiers::SUPER;
        assert_eq!(set.rendered_label().as_deref(), Some("Super+Ctrl+Shift"));
        assert_eq!(Modifiers::empty().rendered_label(), None);
    }
}
