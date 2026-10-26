//! Per-device event haptics: a short waveform on the device's own haptic
//! motor for ordinary input — a button press, a wheel detent, a recognized
//! gesture, a drag — and for what that input changes, in every application.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::button_map::deserialize_button_map;
use crate::binding::ButtonId;

/// What one input plays.
///
/// Rides the IPC wire (the agent's `play_haptic`), so variants are
/// append-only. Deliberately only the short collision waveforms: event haptics answer
/// input the user gives constantly, so the alert patterns the engine also
/// carries (alerts, jingles, ringing) would be relentless. Named as Logi
/// Options+ names them; the agent maps each to its HID++ `0x19b0` ID.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventHapticFeedback {
    /// Nothing plays.
    #[default]
    Off,
    /// The faintest tap the engine plays.
    WhisperCollision,
    /// A soft tap, a little firmer than [`Self::WhisperCollision`].
    SubtleCollision,
    /// A dull, damped knock.
    DampCollision,
    /// A crisp, hard knock.
    SharpCollision,
}

impl EventHapticFeedback {
    /// Every choice, off first and then faintest first.
    pub const ALL: [Self; 5] = [
        Self::Off,
        Self::WhisperCollision,
        Self::SubtleCollision,
        Self::DampCollision,
        Self::SharpCollision,
    ];

    /// Whether this plays anything.
    #[must_use]
    pub fn plays(self) -> bool {
        self != Self::Off
    }

    /// The name `config.toml` and the CLI use.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::WhisperCollision => "whisper_collision",
            Self::SubtleCollision => "subtle_collision",
            Self::DampCollision => "damp_collision",
            Self::SharpCollision => "sharp_collision",
        }
    }

    /// The choice [`Self::name`] names.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|feedback| feedback.name() == name)
    }
}

/// The buttons event haptics can answer, in the order settings list them.
///
/// The haptic panel is left out: it opens the Actions Ring, which plays its
/// own haptics.
pub const EVENT_HAPTIC_BUTTONS: [ButtonId; 6] = [
    ButtonId::LeftClick,
    ButtonId::RightClick,
    ButtonId::MiddleClick,
    ButtonId::Back,
    ButtonId::Forward,
    ButtonId::GestureButton,
];

/// One input event haptics can answer, as settings list it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventHapticInput {
    /// A press of this button.
    Button(ButtonId),
    /// One detent of the main wheel.
    Wheel,
    /// One step of the thumb wheel, which has no detents of its own.
    Thumbwheel,
    /// A gesture recognized and its action run.
    Gestures,
    /// The device changed mode: SmartShift ratchet or free spin, a DPI step.
    ModeChanges,
    /// One step of the output volume.
    LevelSteps,
    /// The output volume reached 0 or 100 %.
    LevelLimits,
    /// The pointer moved onto another display.
    DisplayCrossing,
    /// A drag picked something up, or dropped it.
    Drag,
    /// A copy or cut put something on the clipboard.
    Copy,
}

impl EventHapticInput {
    /// Every input that is not a button, in the order settings list them.
    pub const NON_BUTTONS: [Self; 9] = [
        Self::Wheel,
        Self::Thumbwheel,
        Self::Gestures,
        Self::ModeChanges,
        Self::LevelSteps,
        Self::LevelLimits,
        Self::DisplayCrossing,
        Self::Drag,
        Self::Copy,
    ];
}

/// Event haptics settings for one device. Off unless the user turns them on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventHapticsConfig {
    /// Whether this device plays event haptics at all.
    #[serde(default)]
    pub enabled: bool,
    /// What a press of each button plays. A button not listed plays nothing.
    #[serde(
        default = "default_buttons",
        deserialize_with = "deserialize_button_map"
    )]
    pub buttons: BTreeMap<ButtonId, EventHapticFeedback>,
    /// What a main-wheel detent plays.
    #[serde(default = "default_wheel")]
    pub wheel: EventHapticFeedback,
    /// What a thumb-wheel step plays.
    #[serde(default = "default_subtle")]
    pub thumbwheel: EventHapticFeedback,
    /// What a recognized gesture plays.
    #[serde(default = "default_subtle")]
    pub gestures: EventHapticFeedback,
    /// What a SmartShift or DPI change plays. A DPI step plays once per
    /// preset position, so the count tells which preset is active.
    #[serde(default = "default_wheel")]
    pub mode_changes: EventHapticFeedback,
    /// What a volume step plays.
    #[serde(default = "default_whisper")]
    pub level_steps: EventHapticFeedback,
    /// What pushing the volume past 0 or 100 % plays.
    #[serde(default = "default_sharp")]
    pub level_limits: EventHapticFeedback,
    /// What moving the pointer onto another display plays. Off by default:
    /// it answers ordinary pointer movement rather than a deliberate act.
    #[serde(default)]
    pub display_crossing: EventHapticFeedback,
    /// What a drag's pick-up and drop play. Off by default.
    #[serde(default)]
    pub drag: EventHapticFeedback,
    /// What a copy or cut to the clipboard plays. Off by default.
    #[serde(default)]
    pub copy: EventHapticFeedback,
}

impl Default for EventHapticsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            buttons: default_buttons(),
            wheel: default_wheel(),
            thumbwheel: default_subtle(),
            gestures: default_subtle(),
            mode_changes: default_wheel(),
            level_steps: default_whisper(),
            level_limits: default_sharp(),
            display_crossing: EventHapticFeedback::Off,
            drag: EventHapticFeedback::Off,
            copy: EventHapticFeedback::Off,
        }
    }
}

impl EventHapticsConfig {
    /// Whether this value is exactly the implicit default and can be omitted
    /// from `config.toml`.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    /// What a press of `button` plays.
    #[must_use]
    pub fn button(&self, button: ButtonId) -> EventHapticFeedback {
        self.buttons
            .get(&button)
            .copied()
            .unwrap_or(EventHapticFeedback::Off)
    }

    /// What `input` plays.
    #[must_use]
    pub fn feedback(&self, input: EventHapticInput) -> EventHapticFeedback {
        match input {
            EventHapticInput::Button(button) => self.button(button),
            EventHapticInput::Wheel => self.wheel,
            EventHapticInput::Thumbwheel => self.thumbwheel,
            EventHapticInput::Gestures => self.gestures,
            EventHapticInput::ModeChanges => self.mode_changes,
            EventHapticInput::LevelSteps => self.level_steps,
            EventHapticInput::LevelLimits => self.level_limits,
            EventHapticInput::DisplayCrossing => self.display_crossing,
            EventHapticInput::Drag => self.drag,
            EventHapticInput::Copy => self.copy,
        }
    }

    /// Choose what `input` plays.
    pub fn set_feedback(&mut self, input: EventHapticInput, feedback: EventHapticFeedback) {
        let slot = match input {
            EventHapticInput::Button(button) => {
                self.buttons.insert(button, feedback);
                return;
            }
            EventHapticInput::Wheel => &mut self.wheel,
            EventHapticInput::Thumbwheel => &mut self.thumbwheel,
            EventHapticInput::Gestures => &mut self.gestures,
            EventHapticInput::ModeChanges => &mut self.mode_changes,
            EventHapticInput::LevelSteps => &mut self.level_steps,
            EventHapticInput::LevelLimits => &mut self.level_limits,
            EventHapticInput::DisplayCrossing => &mut self.display_crossing,
            EventHapticInput::Drag => &mut self.drag,
            EventHapticInput::Copy => &mut self.copy,
        };
        *slot = feedback;
    }
}

/// The primary buttons are pressed most often, so they get the faintest tap;
/// the rest stay silent until the user picks something for them.
fn default_buttons() -> BTreeMap<ButtonId, EventHapticFeedback> {
    [ButtonId::LeftClick, ButtonId::RightClick]
        .into_iter()
        .map(|button| (button, EventHapticFeedback::WhisperCollision))
        .collect()
}

fn default_wheel() -> EventHapticFeedback {
    EventHapticFeedback::DampCollision
}

fn default_whisper() -> EventHapticFeedback {
    EventHapticFeedback::WhisperCollision
}

fn default_subtle() -> EventHapticFeedback {
    EventHapticFeedback::SubtleCollision
}

fn default_sharp() -> EventHapticFeedback {
    EventHapticFeedback::SharpCollision
}

#[cfg(test)]
mod tests {
    use super::{ButtonId, EventHapticFeedback, EventHapticsConfig};

    #[test]
    fn an_empty_table_is_the_default() {
        let config: EventHapticsConfig = toml::from_str("").expect("an empty table parses");
        assert_eq!(config, EventHapticsConfig::default());
        assert!(config.is_default());
        assert!(!config.enabled);
        assert_eq!(
            config.button(ButtonId::LeftClick),
            EventHapticFeedback::WhisperCollision
        );
        assert_eq!(config.button(ButtonId::Back), EventHapticFeedback::Off);
        assert_eq!(config.wheel, EventHapticFeedback::DampCollision);
    }

    #[test]
    fn buttons_and_the_wheel_use_logitech_waveform_names() {
        let config: EventHapticsConfig = toml::from_str(
            "enabled = true\nwheel = \"off\"\n[buttons]\nBack = \"sharp_collision\"\nLeftClick = \"off\"",
        )
        .expect("named waveforms parse");
        assert_eq!(
            config.button(ButtonId::Back),
            EventHapticFeedback::SharpCollision
        );
        assert_eq!(config.button(ButtonId::LeftClick), EventHapticFeedback::Off);
        assert_eq!(
            config.button(ButtonId::RightClick),
            EventHapticFeedback::Off,
            "a written table replaces the default one"
        );
        assert!(!config.wheel.plays());
    }

    #[test]
    fn a_saved_config_round_trips() {
        let mut config = EventHapticsConfig {
            enabled: true,
            ..EventHapticsConfig::default()
        };
        config.buttons.insert(
            ButtonId::GestureButton,
            EventHapticFeedback::SubtleCollision,
        );
        let text = toml::to_string(&config).expect("serializes");
        let back: EventHapticsConfig = toml::from_str(&text).expect("parses back");
        assert_eq!(back, config);
    }

    #[test]
    fn every_input_reads_back_what_was_set() {
        use super::EventHapticInput;
        let mut config = EventHapticsConfig::default();
        let inputs = EventHapticInput::NON_BUTTONS
            .into_iter()
            .chain([EventHapticInput::Button(ButtonId::Forward)]);
        for input in inputs {
            config.set_feedback(input, EventHapticFeedback::SharpCollision);
            assert_eq!(
                config.feedback(input),
                EventHapticFeedback::SharpCollision,
                "{input:?}"
            );
            config.set_feedback(input, EventHapticFeedback::Off);
            assert_eq!(
                config.feedback(input),
                EventHapticFeedback::Off,
                "{input:?}"
            );
        }
    }

    #[test]
    fn names_are_the_config_spellings() {
        for feedback in EventHapticFeedback::ALL {
            let text = toml::to_string(&EventHapticsConfig {
                wheel: feedback,
                ..EventHapticsConfig::default()
            })
            .expect("serializes");
            assert!(
                text.contains(&format!("wheel = \"{}\"", feedback.name())),
                "{text}"
            );
            assert_eq!(
                EventHapticFeedback::from_name(feedback.name()),
                Some(feedback)
            );
        }
        assert_eq!(EventHapticFeedback::from_name("ringing"), None);
    }

    #[test]
    fn alert_waveforms_and_unknown_keys_are_refused() {
        assert!(
            toml::from_str::<EventHapticsConfig>("wheel = \"ringing\"").is_err(),
            "an alert pattern must not be a wheel haptic"
        );
        assert!(
            toml::from_str::<EventHapticsConfig>("intensity = 40").is_err(),
            "an unknown key must fail loudly"
        );
    }
}
