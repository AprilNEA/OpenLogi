//! Lighting catalog and capability snapshot that cross the agent↔GUI IPC.
//!
//! HID++ writes live in `openlogi-device`. This module is the persisted effect
//! vocabulary, the G HUB-equivalent prefab table, and the per-device
//! [`LightingInfo`] the GUI uses to filter tiles.

use serde::{Deserialize, Serialize};

use super::OnboardLedMode;

/// A lighting effect the user can pick. Variant order is wire format — append
/// only. Unknown TOML values are rejected (`deny_unknown_fields` on
/// [`super::super::config::Lighting`] still applies to the parent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LightingEffect {
    /// Solid colour (HID++ fixed).
    #[default]
    Solid,
    /// Spectrum colour cycle.
    ColorCycle,
    /// Travelling colour wave.
    ColorWave,
    /// Twinkling starlight.
    Starlight,
    /// Breathing / pulse.
    Breathing,
    /// Ripple from a press origin.
    Ripple,
    /// Keys light on press (firmware).
    LightOnPress,
    /// Host: sample the primary display.
    ScreenSampler,
    /// Host: audio reactive.
    AudioVisualizer,
    /// Host or firmware: flash on input.
    EchoPress,
    /// Host per-key: travelling ocean hue.
    Ocean,
    /// Host per-key: lightning flashes.
    Lightning,
    /// Host per-key: vertical colour wash.
    VerticalFade,
    /// Host per-key: high-contrast bands.
    Contrast,
    /// Host per-key: red / white / blue cycle.
    RedWhiteBlue,
    /// Host per-key: sparse twinkling stars.
    SmoothStars,
    /// Host per-key: smooth travelling wave.
    SmoothWave,
    /// Host per-key: expanding pulsar points.
    Pulsar,
    /// Host zonal or per-key: spectrum pulse.
    SpectrumPulse,
    /// Host per-key: neon sweep.
    Neon,
    /// Host per-key: sparse starfield.
    OuterSpace,
    /// Host per-key: tidal wash.
    Tide,
}

impl LightingEffect {
    /// English catalog key (the GUI `tr!` source string).
    #[must_use]
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Solid => "Solid color",
            Self::ColorCycle => "Color cycle",
            Self::ColorWave => "Color wave",
            Self::Starlight => "Starlight",
            Self::Breathing => "Breathing",
            Self::Ripple => "Ripple",
            Self::LightOnPress => "Light on press",
            Self::ScreenSampler => "Screen sampler",
            Self::AudioVisualizer => "Audio visualizer",
            Self::EchoPress => "Echo press",
            Self::Ocean => "Ocean",
            Self::Lightning => "Lightning",
            Self::VerticalFade => "Vertical fade",
            Self::Contrast => "Contrast",
            Self::RedWhiteBlue => "Red white blue",
            Self::SmoothStars => "Smooth stars",
            Self::SmoothWave => "Smooth wave",
            Self::Pulsar => "Pulsar",
            Self::SpectrumPulse => "Spectrum pulse",
            Self::Neon => "Neon",
            Self::OuterSpace => "Outer space",
            Self::Tide => "Tide",
        }
    }

    /// HID++ `0x8070` effect id when this prefab is firmware-driven.
    #[must_use]
    pub const fn firmware_id(self) -> Option<u16> {
        match self {
            Self::Solid => Some(1),
            Self::ColorCycle => Some(3),
            Self::ColorWave => Some(4),
            Self::Starlight => Some(5),
            Self::LightOnPress => Some(6),
            Self::Breathing => Some(10),
            Self::Ripple => Some(11),
            _ => None,
        }
    }

    /// Whether the agent must keep a host renderer running.
    ///
    /// [`Self::EchoPress`] is resolved at apply time: firmware `LightOnPress`
    /// when the device advertises it, otherwise a host flash.
    #[must_use]
    pub const fn is_host(self) -> bool {
        self.firmware_id().is_none() && !matches!(self, Self::EchoPress)
    }

    /// Snake-case id used in TOML and `diag lighting --effect`.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Solid => "solid",
            Self::ColorCycle => "color_cycle",
            Self::ColorWave => "color_wave",
            Self::Starlight => "starlight",
            Self::Breathing => "breathing",
            Self::Ripple => "ripple",
            Self::LightOnPress => "light_on_press",
            Self::ScreenSampler => "screen_sampler",
            Self::AudioVisualizer => "audio_visualizer",
            Self::EchoPress => "echo_press",
            Self::Ocean => "ocean",
            Self::Lightning => "lightning",
            Self::VerticalFade => "vertical_fade",
            Self::Contrast => "contrast",
            Self::RedWhiteBlue => "red_white_blue",
            Self::SmoothStars => "smooth_stars",
            Self::SmoothWave => "smooth_wave",
            Self::Pulsar => "pulsar",
            Self::SpectrumPulse => "spectrum_pulse",
            Self::Neon => "neon",
            Self::OuterSpace => "outer_space",
            Self::Tide => "tide",
        }
    }

    /// Parse a snake-case catalog id.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        LIGHTING_PREFABS
            .iter()
            .map(|prefab| prefab.effect)
            .find(|effect| effect.id() == id)
    }

    /// Prefab metadata for this effect.
    #[must_use]
    pub const fn prefab(self) -> LightingPrefab {
        match self {
            Self::Solid => LIGHTING_PREFABS[0],
            Self::ColorCycle => LIGHTING_PREFABS[1],
            Self::Breathing => LIGHTING_PREFABS[2],
            Self::Starlight => LIGHTING_PREFABS[3],
            Self::ColorWave => LIGHTING_PREFABS[4],
            Self::Ripple => LIGHTING_PREFABS[5],
            Self::LightOnPress => LIGHTING_PREFABS[6],
            Self::EchoPress => LIGHTING_PREFABS[7],
            Self::ScreenSampler => LIGHTING_PREFABS[8],
            Self::AudioVisualizer => LIGHTING_PREFABS[9],
            Self::Ocean => LIGHTING_PREFABS[10],
            Self::Lightning => LIGHTING_PREFABS[11],
            Self::VerticalFade => LIGHTING_PREFABS[12],
            Self::Contrast => LIGHTING_PREFABS[13],
            Self::RedWhiteBlue => LIGHTING_PREFABS[14],
            Self::SmoothStars => LIGHTING_PREFABS[15],
            Self::SmoothWave => LIGHTING_PREFABS[16],
            Self::Pulsar => LIGHTING_PREFABS[17],
            Self::SpectrumPulse => LIGHTING_PREFABS[18],
            Self::Neon => LIGHTING_PREFABS[19],
            Self::OuterSpace => LIGHTING_PREFABS[20],
            Self::Tide => LIGHTING_PREFABS[21],
        }
    }

    /// Map an onboard-profile LED mode onto a catalog effect.
    #[must_use]
    pub const fn from_onboard(mode: OnboardLedMode) -> Option<Self> {
        match mode {
            OnboardLedMode::On => Some(Self::Solid),
            OnboardLedMode::Cycle => Some(Self::ColorCycle),
            OnboardLedMode::ColorWave => Some(Self::ColorWave),
            OnboardLedMode::Starlight => Some(Self::Starlight),
            OnboardLedMode::Breathing => Some(Self::Breathing),
            OnboardLedMode::Ripple => Some(Self::Ripple),
            OnboardLedMode::Off | OnboardLedMode::Custom => None,
        }
    }
}

/// Static description of one catalog tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent capability flags for one catalog row"
)]
pub struct LightingPrefab {
    /// Catalog id.
    pub effect: LightingEffect,
    /// Colour swatches apply.
    pub has_color: bool,
    /// Speed slider applies.
    pub has_speed: bool,
    /// Brightness slider applies.
    pub has_brightness: bool,
    /// Offer on mouse zonal devices (`MOUSE_RGB_ZONAL`).
    pub mouse_zonal: bool,
    /// Offer on keyboard zonal devices.
    pub keyboard_zonal: bool,
    /// Offer on per-key keyboards.
    pub keyboard_per_key: bool,
}

/// Physical zone location from `0x8070` `getZoneInfo`. Variant order is wire
/// format — append only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LightingZoneLocation {
    /// Firmware reported a location this build does not name.
    Unknown,
    /// Primary / body.
    Primary,
    /// Logo.
    Logo,
    /// Left side.
    LeftSide,
    /// Right side.
    RightSide,
    /// Combined zone.
    Combined,
    /// Numbered primary 1.
    Primary1,
    /// Numbered primary 2.
    Primary2,
    /// Numbered primary 3.
    Primary3,
    /// Numbered primary 4.
    Primary4,
    /// Numbered primary 5.
    Primary5,
    /// Numbered primary 6.
    Primary6,
}

impl LightingZoneLocation {
    /// Map a HID++ `LocationEffect` discriminant.
    #[must_use]
    pub const fn from_hidpp(location: u16) -> Self {
        match location {
            1 => Self::Primary,
            2 => Self::Logo,
            3 => Self::LeftSide,
            4 => Self::RightSide,
            5 => Self::Combined,
            6 => Self::Primary1,
            7 => Self::Primary2,
            8 => Self::Primary3,
            9 => Self::Primary4,
            10 => Self::Primary5,
            11 => Self::Primary6,
            _ => Self::Unknown,
        }
    }

    /// English catalog key for this zone.
    #[must_use]
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Unknown => "Zone",
            Self::Primary => "Primary",
            Self::Logo => "Logo",
            Self::LeftSide => "Left side",
            Self::RightSide => "Right side",
            Self::Combined => "Combined",
            Self::Primary1 => "Primary 1",
            Self::Primary2 => "Primary 2",
            Self::Primary3 => "Primary 3",
            Self::Primary4 => "Primary 4",
            Self::Primary5 => "Primary 5",
            Self::Primary6 => "Primary 6",
        }
    }
}

/// One `0x8070` LED zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LightingZone {
    /// Zone index passed to `setZoneEffect`.
    pub index: u8,
    /// Physical location.
    pub location: LightingZoneLocation,
    /// `EffectId` values this zone advertises (excluding Disabled).
    pub firmware_effects: Vec<u16>,
}

/// Runtime lighting capabilities for the Lighting tab. Field order is wire
/// format — append only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent device and host-backend flags; field order is wire format"
)]
pub struct LightingInfo {
    /// The device is a mouse/trackball (zonal catalog), not a keyboard.
    pub mouse: bool,
    /// `0x8080` / `0x8081` per-key zones are present.
    pub per_key: bool,
    /// `0x8070` zones. Empty when the effect engine is absent.
    pub zones: Vec<LightingZone>,
    /// The agent compiled and initialized a screen-sampler backend.
    pub screen_sampler: bool,
    /// The agent compiled and initialized an audio-visualizer backend.
    pub audio_visualizer: bool,
}

impl LightingInfo {
    /// Firmware effect ids advertised on any zone.
    #[must_use]
    pub fn advertised_firmware_ids(&self) -> Vec<u16> {
        let mut ids = Vec::new();
        for zone in &self.zones {
            for id in &zone.firmware_effects {
                if !ids.contains(id) {
                    ids.push(*id);
                }
            }
        }
        ids
    }

    /// Prefabs the Lighting tab should show for this device.
    #[must_use]
    pub fn available_prefabs(&self) -> Vec<LightingPrefab> {
        let advertised = self.advertised_firmware_ids();
        LIGHTING_PREFABS
            .iter()
            .copied()
            .filter(|prefab| self.prefab_is_available(*prefab, &advertised))
            .collect()
    }

    fn prefab_is_available(&self, prefab: LightingPrefab, advertised: &[u16]) -> bool {
        let class_ok = if self.mouse {
            prefab.mouse_zonal
        } else if self.per_key {
            prefab.keyboard_per_key || prefab.keyboard_zonal
        } else {
            prefab.keyboard_zonal
        };
        if !class_ok {
            return false;
        }
        if let Some(id) = prefab.effect.firmware_id() {
            return advertised.contains(&id);
        }
        match prefab.effect {
            LightingEffect::ScreenSampler => self.screen_sampler,
            LightingEffect::AudioVisualizer => self.audio_visualizer,
            LightingEffect::EchoPress => {
                advertised.contains(&6) || self.per_key || !self.zones.is_empty()
            }
            LightingEffect::Ocean
            | LightingEffect::Lightning
            | LightingEffect::VerticalFade
            | LightingEffect::Contrast
            | LightingEffect::RedWhiteBlue
            | LightingEffect::SmoothStars
            | LightingEffect::SmoothWave
            | LightingEffect::Pulsar
            | LightingEffect::Neon
            | LightingEffect::OuterSpace
            | LightingEffect::Tide => self.per_key,
            LightingEffect::SpectrumPulse => self.per_key || !self.zones.is_empty(),
            LightingEffect::Solid
            | LightingEffect::ColorCycle
            | LightingEffect::ColorWave
            | LightingEffect::Starlight
            | LightingEffect::Breathing
            | LightingEffect::Ripple
            | LightingEffect::LightOnPress => false,
        }
    }
}

/// Default speed percent for new lighting configs.
#[must_use]
pub const fn default_lighting_speed() -> u8 {
    50
}

/// Map a 0–100 speed slider to an effect period in milliseconds.
#[must_use]
pub fn speed_to_period_ms(speed: u8) -> u16 {
    let clamped = u32::from(speed.min(100));
    let period = 3000u32.saturating_sub(clamped.saturating_mul(3000 - 150) / 100);
    u16::try_from(period.max(150)).unwrap_or(150)
}

/// Firmware intensity byte: `0` means 100%, otherwise `1..=100`.
#[must_use]
pub const fn firmware_intensity(brightness: u8) -> u8 {
    if brightness >= 100 {
        0
    } else if brightness == 0 {
        1
    } else {
        brightness
    }
}

const ZONAL: bool = true;
const PER_KEY: bool = true;
const NO: bool = false;

/// G HUB-equivalent catalog. Order is the tile order on the Lighting tab.
pub const LIGHTING_PREFABS: &[LightingPrefab] = &[
    prefab(
        LightingEffect::Solid,
        true,
        false,
        true,
        ZONAL,
        ZONAL,
        PER_KEY,
    ),
    prefab(
        LightingEffect::ColorCycle,
        false,
        true,
        true,
        ZONAL,
        ZONAL,
        PER_KEY,
    ),
    prefab(
        LightingEffect::Breathing,
        true,
        true,
        true,
        ZONAL,
        ZONAL,
        PER_KEY,
    ),
    prefab(
        LightingEffect::Starlight,
        true,
        true,
        true,
        ZONAL,
        ZONAL,
        PER_KEY,
    ),
    prefab(
        LightingEffect::ColorWave,
        false,
        true,
        true,
        NO,
        ZONAL,
        PER_KEY,
    ),
    prefab(LightingEffect::Ripple, true, true, true, NO, NO, PER_KEY),
    prefab(
        LightingEffect::LightOnPress,
        true,
        false,
        true,
        NO,
        NO,
        PER_KEY,
    ),
    prefab(
        LightingEffect::EchoPress,
        true,
        true,
        true,
        ZONAL,
        ZONAL,
        PER_KEY,
    ),
    prefab(
        LightingEffect::ScreenSampler,
        false,
        false,
        true,
        ZONAL,
        ZONAL,
        PER_KEY,
    ),
    prefab(
        LightingEffect::AudioVisualizer,
        false,
        false,
        true,
        ZONAL,
        ZONAL,
        PER_KEY,
    ),
    prefab(LightingEffect::Ocean, true, true, true, NO, NO, PER_KEY),
    prefab(LightingEffect::Lightning, true, true, true, NO, NO, PER_KEY),
    prefab(
        LightingEffect::VerticalFade,
        true,
        true,
        true,
        NO,
        NO,
        PER_KEY,
    ),
    prefab(LightingEffect::Contrast, false, true, true, NO, NO, PER_KEY),
    prefab(
        LightingEffect::RedWhiteBlue,
        false,
        true,
        true,
        NO,
        NO,
        PER_KEY,
    ),
    prefab(
        LightingEffect::SmoothStars,
        true,
        true,
        true,
        NO,
        NO,
        PER_KEY,
    ),
    prefab(
        LightingEffect::SmoothWave,
        true,
        true,
        true,
        NO,
        NO,
        PER_KEY,
    ),
    prefab(LightingEffect::Pulsar, true, true, true, NO, NO, PER_KEY),
    prefab(
        LightingEffect::SpectrumPulse,
        false,
        true,
        true,
        ZONAL,
        ZONAL,
        PER_KEY,
    ),
    prefab(LightingEffect::Neon, true, true, true, NO, NO, PER_KEY),
    prefab(
        LightingEffect::OuterSpace,
        true,
        true,
        true,
        NO,
        NO,
        PER_KEY,
    ),
    prefab(LightingEffect::Tide, true, true, true, NO, NO, PER_KEY),
];

#[expect(
    clippy::fn_params_excessive_bools,
    reason = "one catalog row's independent capability flags"
)]
const fn prefab(
    effect: LightingEffect,
    has_color: bool,
    has_speed: bool,
    has_brightness: bool,
    mouse_zonal: bool,
    keyboard_zonal: bool,
    keyboard_per_key: bool,
) -> LightingPrefab {
    LightingPrefab {
        effect,
        has_color,
        has_speed,
        has_brightness,
        mouse_zonal,
        keyboard_zonal,
        keyboard_per_key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mouse_info(ids: &[u16]) -> LightingInfo {
        LightingInfo {
            mouse: true,
            per_key: false,
            zones: vec![LightingZone {
                index: 0,
                location: LightingZoneLocation::Primary,
                firmware_effects: ids.to_vec(),
            }],
            screen_sampler: true,
            audio_visualizer: true,
        }
    }

    fn keyboard_info(ids: &[u16], per_key: bool) -> LightingInfo {
        LightingInfo {
            mouse: false,
            per_key,
            zones: vec![LightingZone {
                index: 0,
                location: LightingZoneLocation::Primary,
                firmware_effects: ids.to_vec(),
            }],
            screen_sampler: true,
            audio_visualizer: true,
        }
    }

    #[test]
    fn g502_hides_keyboard_only_firmware_and_shows_host_zonal() {
        let info = mouse_info(&[1, 3, 5, 10]);
        let effects: Vec<_> = info
            .available_prefabs()
            .into_iter()
            .map(|prefab| prefab.effect)
            .collect();
        assert!(effects.contains(&LightingEffect::Solid));
        assert!(effects.contains(&LightingEffect::ColorCycle));
        assert!(effects.contains(&LightingEffect::Breathing));
        assert!(effects.contains(&LightingEffect::Starlight));
        assert!(effects.contains(&LightingEffect::ScreenSampler));
        assert!(effects.contains(&LightingEffect::AudioVisualizer));
        assert!(effects.contains(&LightingEffect::SpectrumPulse));
        assert!(!effects.contains(&LightingEffect::ColorWave));
        assert!(!effects.contains(&LightingEffect::Ripple));
        assert!(!effects.contains(&LightingEffect::Ocean));
    }

    #[test]
    fn g513_shows_wave_ripple_and_per_key_shows() {
        let info = keyboard_info(&[1, 3, 4, 5, 6, 10, 11], true);
        let effects: Vec<_> = info
            .available_prefabs()
            .into_iter()
            .map(|prefab| prefab.effect)
            .collect();
        assert!(effects.contains(&LightingEffect::ColorWave));
        assert!(effects.contains(&LightingEffect::Ripple));
        assert!(effects.contains(&LightingEffect::Ocean));
        assert!(effects.contains(&LightingEffect::Tide));
        assert!(effects.contains(&LightingEffect::LightOnPress));
    }

    #[test]
    fn host_tiles_hide_when_backends_are_missing() {
        let mut info = mouse_info(&[1]);
        info.screen_sampler = false;
        info.audio_visualizer = false;
        let effects: Vec<_> = info
            .available_prefabs()
            .into_iter()
            .map(|prefab| prefab.effect)
            .collect();
        assert!(!effects.contains(&LightingEffect::ScreenSampler));
        assert!(!effects.contains(&LightingEffect::AudioVisualizer));
        assert!(effects.contains(&LightingEffect::Solid));
    }

    #[test]
    fn speed_maps_onto_the_firmware_period_range() {
        assert_eq!(speed_to_period_ms(0), 3000);
        assert_eq!(speed_to_period_ms(100), 150);
        assert_eq!(firmware_intensity(100), 0);
        assert_eq!(firmware_intensity(40), 40);
    }

    #[test]
    fn onboard_modes_seed_catalog_effects() {
        assert_eq!(
            LightingEffect::from_onboard(OnboardLedMode::Breathing),
            Some(LightingEffect::Breathing)
        );
        assert_eq!(LightingEffect::from_onboard(OnboardLedMode::Off), None);
    }

    #[test]
    fn catalog_ids_round_trip() {
        for prefab in LIGHTING_PREFABS {
            assert_eq!(
                LightingEffect::from_id(prefab.effect.id()),
                Some(prefab.effect)
            );
        }
        assert_eq!(LightingEffect::from_id("nope"), None);
    }
}
