//! Broadcast events emitted by `SolarKeyboardDashboard` (`0x4301`).

/// Battery and light readings shared by every solar-dashboard event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct SolarStatus {
    /// Remaining battery capacity, as a percentage.
    pub battery_level: u8,
    /// Current light measure in lux (`0..=511`).
    pub light_level: u16,
}

impl SolarStatus {
    fn from_payload(payload: &[u8; 16]) -> Self {
        Self {
            battery_level: payload[0],
            light_level: u16::from_be_bytes([payload[1], payload[2]]),
        }
    }
}

/// An event emitted by [`SolarDashboardFeature`](super::SolarDashboardFeature).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum SolarEvent {
    /// Spontaneous battery report (every ~90 s, at power-up, and on reconnect).
    ///
    /// The light level is always `0` for this event.
    Battery(SolarStatus),
    /// Battery and light report sent per the
    /// [`set_light_measure`](super::SolarDashboardFeature::set_light_measure)
    /// schedule.
    LightMeasure(SolarStatus),
    /// The CheckLight button was pressed; carries the latest battery and light
    /// readings.
    CheckLightButton(SolarStatus),
}

/// Decodes a `0x4301` broadcast event payload by its sub-id.
pub(super) fn decode_event(sub_id: u8, payload: &[u8; 16]) -> Option<SolarEvent> {
    let status = SolarStatus::from_payload(payload);
    match sub_id {
        0 => Some(SolarEvent::Battery(status)),
        1 => Some(SolarEvent::LightMeasure(status)),
        2 => Some(SolarEvent::CheckLightButton(status)),
        _ => None,
    }
}
