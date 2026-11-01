//! The programmable top row: Esc, then F1–F21.
//!
//! These are the only keys a [`KeyTrigger`](super::KeyTrigger) can name, and
//! every process that touches one speaks the same three vocabularies for
//! them: the macOS virtual keycode the OS hook reports on every platform, the
//! lowercase name a trigger is written with in `config.toml`, and the legend
//! printed on the key. The table lives here once — a second copy that drifts
//! makes every saved binding miss.

/// A key of the programmable top row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FunctionKey {
    /// Escape.
    Esc,
    /// F1.
    F1,
    /// F2.
    F2,
    /// F3.
    F3,
    /// F4.
    F4,
    /// F5.
    F5,
    /// F6.
    F6,
    /// F7.
    F7,
    /// F8.
    F8,
    /// F9.
    F9,
    /// F10.
    F10,
    /// F11.
    F11,
    /// F12.
    F12,
    /// F13.
    F13,
    /// F14.
    F14,
    /// F15.
    F15,
    /// F16.
    F16,
    /// F17.
    F17,
    /// F18.
    F18,
    /// F19.
    F19,
    /// F20.
    F20,
    /// F21. macOS has no `kVK_*` for it, so its keycode is OpenLogi's own.
    F21,
}

impl FunctionKey {
    /// Every key, in the order the row is laid out.
    pub const ALL: [Self; 22] = [
        Self::Esc,
        Self::F1,
        Self::F2,
        Self::F3,
        Self::F4,
        Self::F5,
        Self::F6,
        Self::F7,
        Self::F8,
        Self::F9,
        Self::F10,
        Self::F11,
        Self::F12,
        Self::F13,
        Self::F14,
        Self::F15,
        Self::F16,
        Self::F17,
        Self::F18,
        Self::F19,
        Self::F20,
        Self::F21,
    ];

    /// The macOS virtual keycode (`kVK_*`). Key events and triggers carry this
    /// keycode on every platform, so a config written on one host binds the
    /// same key on another.
    #[must_use]
    pub const fn keycode(self) -> u16 {
        match self {
            Self::Esc => 0x35,
            Self::F1 => 0x7A,
            Self::F2 => 0x78,
            Self::F3 => 0x63,
            Self::F4 => 0x76,
            Self::F5 => 0x60,
            Self::F6 => 0x61,
            Self::F7 => 0x62,
            Self::F8 => 0x64,
            Self::F9 => 0x65,
            Self::F10 => 0x6D,
            Self::F11 => 0x67,
            Self::F12 => 0x6F,
            Self::F13 => 0x69,
            Self::F14 => 0x6B,
            Self::F15 => 0x71,
            Self::F16 => 0x6A,
            Self::F17 => 0x40,
            Self::F18 => 0x4F,
            Self::F19 => 0x50,
            Self::F20 => 0x5A,
            // Past the 7-bit `kVK_*` range, so it can never collide with a key
            // the OS reports.
            Self::F21 => 0x100,
        }
    }

    /// The key a macOS virtual keycode names, if it is on this row.
    #[must_use]
    pub fn from_keycode(keycode: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|key| key.keycode() == keycode)
    }

    /// The name a trigger is written with: `esc`, `f1` … `f21`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Esc => "esc",
            Self::F1 => "f1",
            Self::F2 => "f2",
            Self::F3 => "f3",
            Self::F4 => "f4",
            Self::F5 => "f5",
            Self::F6 => "f6",
            Self::F7 => "f7",
            Self::F8 => "f8",
            Self::F9 => "f9",
            Self::F10 => "f10",
            Self::F11 => "f11",
            Self::F12 => "f12",
            Self::F13 => "f13",
            Self::F14 => "f14",
            Self::F15 => "f15",
            Self::F16 => "f16",
            Self::F17 => "f17",
            Self::F18 => "f18",
            Self::F19 => "f19",
            Self::F20 => "f20",
            Self::F21 => "f21",
        }
    }

    /// The key a trigger name spells, case-insensitively.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|key| key.name().eq_ignore_ascii_case(name))
    }

    /// The legend printed on the key: `Esc`, `F1` … `F21`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Esc => "Esc",
            Self::F1 => "F1",
            Self::F2 => "F2",
            Self::F3 => "F3",
            Self::F4 => "F4",
            Self::F5 => "F5",
            Self::F6 => "F6",
            Self::F7 => "F7",
            Self::F8 => "F8",
            Self::F9 => "F9",
            Self::F10 => "F10",
            Self::F11 => "F11",
            Self::F12 => "F12",
            Self::F13 => "F13",
            Self::F14 => "F14",
            Self::F15 => "F15",
            Self::F16 => "F16",
            Self::F17 => "F17",
            Self::F18 => "F18",
            Self::F19 => "F19",
            Self::F20 => "F20",
            Self::F21 => "F21",
        }
    }

    /// The `n`th F key, counting from 1 — how a platform whose own keycodes
    /// number the F keys consecutively finds the key it saw. `None` past F21.
    #[must_use]
    pub fn nth_f(n: u16) -> Option<Self> {
        match n {
            0 => None,
            n => Self::ALL.get(usize::from(n)).copied(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::FunctionKey;

    #[test]
    fn every_key_round_trips_through_each_vocabulary() {
        for key in FunctionKey::ALL {
            assert_eq!(FunctionKey::from_keycode(key.keycode()), Some(key));
            assert_eq!(FunctionKey::from_name(key.name()), Some(key));
        }
        // A keycode or a name entered twice would shadow a key silently.
        let keycodes: BTreeSet<u16> = FunctionKey::ALL.iter().map(|key| key.keycode()).collect();
        let names: BTreeSet<&str> = FunctionKey::ALL.iter().map(|key| key.name()).collect();
        assert_eq!(keycodes.len(), FunctionKey::ALL.len());
        assert_eq!(names.len(), FunctionKey::ALL.len());
    }

    #[test]
    fn the_nth_f_key_counts_from_one_and_stops_at_f21() {
        assert_eq!(FunctionKey::nth_f(0), None, "Esc is not an F key");
        assert_eq!(FunctionKey::nth_f(1), Some(FunctionKey::F1));
        assert_eq!(FunctionKey::nth_f(21), Some(FunctionKey::F21));
        assert_eq!(FunctionKey::nth_f(22), None);
    }

    #[test]
    fn names_parse_case_insensitively() {
        assert_eq!(FunctionKey::from_name("F12"), Some(FunctionKey::F12));
        assert_eq!(FunctionKey::from_name("Esc"), Some(FunctionKey::Esc));
        assert_eq!(FunctionKey::from_name("F21"), Some(FunctionKey::F21));
        assert_eq!(FunctionKey::from_name("f22"), None);
    }
}
