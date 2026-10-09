//! USB keyboard-page shortcuts. Names are physical keys, independent of OS layout.
use openlogi_ipc::gaming::GamingAction;

const KEYS: &[(u8, &str)] = &[
    (40, "Enter"),
    (41, "Esc"),
    (42, "Backspace"),
    (43, "Tab"),
    (44, "Space"),
    (45, "Minus"),
    (46, "Equal"),
    (47, "LeftBracket"),
    (48, "RightBracket"),
    (49, "Backslash"),
    (51, "Semicolon"),
    (52, "Quote"),
    (53, "Backtick"),
    (54, "Comma"),
    (55, "Period"),
    (56, "Slash"),
    (57, "CapsLock"),
    (70, "PrintScreen"),
    (71, "ScrollLock"),
    (72, "Pause"),
    (73, "Insert"),
    (74, "Home"),
    (75, "PageUp"),
    (76, "Delete"),
    (77, "End"),
    (78, "PageDown"),
    (79, "Right"),
    (80, "Left"),
    (81, "Down"),
    (82, "Up"),
    (83, "NumLock"),
    (84, "NumDivide"),
    (85, "NumMultiply"),
    (86, "NumMinus"),
    (87, "NumPlus"),
    (88, "NumEnter"),
    (98, "Num0"),
    (99, "NumPeriod"),
    (101, "Menu"),
];

fn key_name(usage: u8) -> String {
    match usage {
        4..=29 => char::from(b'A' + usage - 4).to_string(),
        30..=38 => (usage - 29).to_string(),
        39 => "0".into(),
        58..=69 => format!("F{}", usage - 57),
        104..=115 => format!("F{}", usage - 91),
        89..=97 => format!("Num{}", usage - 88),
        _ => KEYS
            .iter()
            .find(|(u, _)| *u == usage)
            .map_or_else(|| format!("HID{usage}"), |(_, name)| (*name).into()),
    }
}

pub(super) fn label(usage: u8, modifiers: u8) -> String {
    let mut parts = Vec::new();
    for (bit, name) in [
        "Ctrl", "Shift", "Alt", "Win", "RCtrl", "RShift", "RAlt", "RWin",
    ]
    .iter()
    .enumerate()
    {
        if modifiers & (1 << bit) != 0 {
            parts.push((*name).to_string());
        }
    }
    parts.push(key_name(usage));
    parts.join("+")
}

pub(super) fn parse(text: &str) -> Option<GamingAction> {
    let mut modifiers = 0;
    let mut usage = None;
    for part in text.split('+').map(str::trim) {
        let bit = match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Some(0),
            "shift" => Some(1),
            "alt" => Some(2),
            "win" | "super" | "cmd" => Some(3),
            "rctrl" => Some(4),
            "rshift" => Some(5),
            "ralt" => Some(6),
            "rwin" => Some(7),
            _ => None,
        };
        if let Some(bit) = bit {
            if modifiers & (1 << bit) != 0 {
                return None;
            }
            modifiers |= 1 << bit;
        } else {
            if usage.is_some() {
                return None;
            }
            usage = (4..=231).find(|u| key_name(*u).eq_ignore_ascii_case(part));
            usage?;
        }
    }
    Some(GamingAction::Key {
        usage: usage?,
        modifiers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_round_trip_all_supported_keys_and_modifier_bits() {
        for usage in 4..=231 {
            for modifiers in [0, 1, 2, 4, 8, 16, 32, 64, 128, 255] {
                assert_eq!(
                    parse(&label(usage, modifiers)),
                    Some(GamingAction::Key { usage, modifiers })
                );
            }
        }
    }

    #[test]
    fn rejects_sequences_empty_keys_and_duplicate_modifiers() {
        for text in [
            "",
            "Ctrl",
            "Ctrl+",
            "+A",
            "A+B",
            "Ctrl+Ctrl+A",
            "Ctrl+Coffee",
            "HID255",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
        assert_eq!(
            parse(" ctrl + Shift + F24 "),
            Some(GamingAction::Key {
                usage: 115,
                modifiers: 3
            })
        );
    }
}
