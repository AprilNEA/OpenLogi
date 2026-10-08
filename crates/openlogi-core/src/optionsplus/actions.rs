//! Structured payloads are authoritative; translated labels and task names are
//! not a shortcut parser. Selected gesture cards are converted atomically.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::binding::{Action, Binding, ButtonId, GestureDirection, KeyCombo};

pub(super) fn binding(card: &Value, button: ButtonId) -> Option<Binding> {
    let card = if card["attribute"] == "ONE_OF" {
        let selected = card["selectedNestedCard"].as_str()?;
        card["nestedCards"].get(selected)?
    } else {
        card
    };
    if card["attribute"] == "ADAPTER_4WAYS" {
        if !button.supports_gesture_mode() {
            return None;
        }
        let cards = card["nestedCards"].as_object()?;
        if cards.len() != 5 {
            return None;
        }
        let mut gestures = BTreeMap::new();
        for (name, direction) in [
            ("up", GestureDirection::Up),
            ("down", GestureDirection::Down),
            ("left", GestureDirection::Left),
            ("right", GestureDirection::Right),
            ("click", GestureDirection::Click),
        ] {
            gestures.insert(direction, action(cards.get(name)?)?);
        }
        Some(Binding::Gesture(gestures))
    } else {
        action(card).map(Binding::Single)
    }
}

fn action(card: &Value) -> Option<Action> {
    if card["attribute"] != "MACRO_PLAYBACK" {
        return None;
    }
    let data = &card["macro"];
    // Mode shift's observed macro is empty, but it is not a no-op. Require the
    // exact preset and shape; never infer DoNothing from an empty object.
    if card["id"] == "card_global_presets_mode_shift"
        && card["taskId"] == 157
        && data.as_object().is_some_and(serde_json::Map::is_empty)
    {
        return Some(Action::ToggleSmartShift);
    }
    match data["type"].as_str()? {
        "DO_NOTHING" => Some(Action::None),
        "KEYSTROKE" => {
            #[derive(Deserialize)]
            struct Keystroke {
                code: u8,
                #[serde(default)]
                modifiers: Vec<u8>,
            }
            let key: Keystroke = serde_json::from_value(data["keystroke"].clone()).ok()?;
            KeyCombo::from_hid_usages(key.code, &key.modifiers).map(Action::CustomShortcut)
        }
        "MOUSE" => match data["mouse"]["action"].as_str()? {
            "OSX_GESTURE_BACK" => Some(Action::BrowserBack),
            "OSX_GESTURE_FORWARD" => Some(Action::BrowserForward),
            "BUTTON" => match data["mouse"]["hidUsage"].as_u64()? {
                1 => Some(Action::LeftClick),
                2 => Some(Action::RightClick),
                3 => Some(Action::MiddleClick),
                _ => None,
            },
            _ => None,
        },
        "QUICK_LAUNCH" => match data["quickLaunch"]["action"].as_str()? {
            "MISSION_CONTROL" => Some(Action::MissionControl),
            "APP_EXPOSE" => Some(Action::AppExpose),
            "LAUNCHPAD" => Some(Action::LaunchpadShow),
            _ => None,
        },
        "SYSTEM" => match data["system"]["action"].as_str()? {
            "SWITCH_BETWEEN_DESKTOPS_LEFT" => Some(Action::PreviousDesktop),
            "SWITCH_BETWEEN_DESKTOPS_RIGHT" => Some(Action::NextDesktop),
            _ => None,
        },
        "MEDIA" => match data["media"]["usage"].as_str()? {
            "PLAY_PAUSE" => Some(Action::PlayPause),
            "NEXT_TRACK" => Some(Action::NextTrack),
            "PREVIOUS_TRACK" => Some(Action::PrevTrack),
            "VOLUME_UP" => Some(Action::VolumeUp),
            "VOLUME_DOWN" => Some(Action::VolumeDown),
            "MUTE" => Some(Action::MuteVolume),
            _ => None,
        },
        _ => None,
    }
}

pub(super) fn uses_navigation(binding: &Binding) -> bool {
    let navigation =
        |action: &Action| matches!(action, Action::BrowserBack | Action::BrowserForward);
    match binding {
        Binding::Single(action) => navigation(action),
        Binding::Gesture(actions) => actions.values().any(navigation),
        Binding::LongPress(_) => false,
    }
}
