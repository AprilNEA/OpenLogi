//! Original host lighting algorithms. Coordinates are derived from zone id.

#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    reason = "visual math maps unit intervals and zone ids onto RGB bytes"
)]

use openlogi_core::hid::LightingEffect;

pub struct HostInputs {
    pub screen: Option<(u8, u8, u8)>,
    pub audio_rms: f32,
    pub audio_bands: [f32; 8],
    pub press_age_ms: u64,
}

#[expect(
    clippy::too_many_arguments,
    reason = "one frame is effect + time + colour + zone list"
)]
pub fn frame(
    effect: LightingEffect,
    seconds: f32,
    speed: u8,
    color: (u8, u8, u8),
    brightness: u8,
    zone_ids: &[u8],
    per_key: bool,
    inputs: &HostInputs,
) -> Vec<(u8, u8, u8, u8)> {
    let phase = seconds * speed_hz(speed);
    zone_ids
        .iter()
        .enumerate()
        .map(|(index, &id)| {
            let (x, y) = key_xy(id, index, zone_ids.len(), per_key);
            let rgb = pixel(effect, phase, x, y, color, inputs);
            let (r, g, b) = scale(rgb, brightness);
            (id, r, g, b)
        })
        .collect()
}

fn speed_hz(speed: u8) -> f32 {
    0.15 + (f32::from(speed.min(100)) / 100.0) * 1.85
}

fn key_xy(id: u8, index: usize, count: usize, per_key: bool) -> (f32, f32) {
    if !per_key {
        let span = count.max(1) as f32;
        return ((index as f32 + 0.5) / span, 0.5);
    }
    let col = f32::from(id % 21) / 20.0;
    let row = f32::from(id / 21) / 6.0;
    (col.clamp(0.0, 1.0), row.clamp(0.0, 1.0))
}

fn pixel(
    effect: LightingEffect,
    phase: f32,
    x: f32,
    y: f32,
    color: (u8, u8, u8),
    inputs: &HostInputs,
) -> (u8, u8, u8) {
    match effect {
        LightingEffect::ScreenSampler => inputs.screen.unwrap_or((0, 0, 0)),
        LightingEffect::AudioVisualizer => audio_pixel(x, inputs),
        LightingEffect::EchoPress => echo_pixel(color, inputs.press_age_ms),
        LightingEffect::Ocean => hue_rgb(fract(x * 0.55 + phase * 0.12 + y * 0.2), 0.65, 0.45),
        LightingEffect::Lightning => lightning(phase, x, y, color),
        LightingEffect::VerticalFade => hue_rgb(fract(y + phase * 0.08), 0.7, 0.5),
        LightingEffect::Contrast => contrast(phase, x),
        LightingEffect::RedWhiteBlue => patriotic(phase, x),
        LightingEffect::SmoothStars => stars(phase, x, y, color, 0.08),
        LightingEffect::SmoothWave => {
            mix_rgb(color, hue_rgb(fract(x - phase * 0.2), 0.55, 0.55), 0.35)
        }
        LightingEffect::Pulsar => pulsar(phase, x, y, color),
        LightingEffect::SpectrumPulse => hue_rgb(fract(phase * 0.15 + x * 0.4), 0.85, pulse(phase)),
        LightingEffect::Neon => hue_rgb(fract(x * 0.8 + phase * 0.25), 0.95, 0.55),
        LightingEffect::OuterSpace => stars(phase, x, y, (120, 160, 255), 0.03),
        LightingEffect::Tide => tide(phase, x, y, color),
        LightingEffect::Solid
        | LightingEffect::ColorCycle
        | LightingEffect::ColorWave
        | LightingEffect::Starlight
        | LightingEffect::Breathing
        | LightingEffect::Ripple
        | LightingEffect::LightOnPress => color,
    }
}

fn audio_pixel(x: f32, inputs: &HostInputs) -> (u8, u8, u8) {
    let band = ((x * 8.0).floor() as usize).min(7);
    let level = inputs.audio_bands[band]
        .max(inputs.audio_rms)
        .clamp(0.0, 1.0);
    hue_rgb(0.55 + x * 0.2, 0.85, 0.15 + level * 0.7)
}

fn echo_pixel(color: (u8, u8, u8), age_ms: u64) -> (u8, u8, u8) {
    let fade = if age_ms > 400 {
        0.08
    } else {
        1.0 - (age_ms as f32 / 400.0)
    };
    scale(color, (fade * 100.0) as u8)
}

fn lightning(phase: f32, x: f32, y: f32, color: (u8, u8, u8)) -> (u8, u8, u8) {
    let burst = hash(phase.floor() as u32, (x * 17.0) as u32);
    if burst > 0.92 && (y - burst).abs() < 0.35 {
        (255, 255, 255)
    } else {
        scale(color, 12)
    }
}

fn contrast(phase: f32, x: f32) -> (u8, u8, u8) {
    if ((x * 8.0 + phase).floor() as i32).rem_euclid(2) == 0 {
        (255, 255, 255)
    } else {
        (8, 8, 12)
    }
}

fn patriotic(phase: f32, x: f32) -> (u8, u8, u8) {
    match ((x * 3.0 + phase * 0.4).floor() as i32).rem_euclid(3) {
        0 => (196, 30, 58),
        1 => (255, 255, 255),
        _ => (0, 82, 165),
    }
}

fn stars(phase: f32, x: f32, y: f32, color: (u8, u8, u8), density: f32) -> (u8, u8, u8) {
    let spark = hash((x * 97.0) as u32, (y * 53.0) as u32);
    if spark < density {
        let twinkle = f32::midpoint(1.0, (phase * 6.0 + spark * 20.0).sin());
        scale(color, (20.0 + twinkle * 80.0) as u8)
    } else {
        (4, 6, 14)
    }
}

fn pulsar(phase: f32, x: f32, y: f32, color: (u8, u8, u8)) -> (u8, u8, u8) {
    let dx = x - 0.5;
    let dy = y - 0.5;
    let dist = (dx * dx + dy * dy).sqrt();
    let ring = (1.0 - ((dist * 4.0 - fract(phase)).abs() * 6.0).min(1.0)).max(0.0);
    scale(color, (10.0 + ring * 90.0) as u8)
}

fn tide(phase: f32, x: f32, y: f32, color: (u8, u8, u8)) -> (u8, u8, u8) {
    let level = f32::midpoint(1.0, (phase * 1.4 + x * 2.0).sin());
    if y > 1.0 - level {
        mix_rgb(color, hue_rgb(0.55, 0.6, 0.45), 0.35)
    } else {
        (6, 10, 22)
    }
}

fn pulse(phase: f32) -> f32 {
    0.35 + (1.0 + (phase * std::f32::consts::TAU).sin()) * 0.3
}

fn hue_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let h = fract(h) * 6.0;
    let i = h.floor();
    let f = h - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match i as i32 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

fn scale(color: (u8, u8, u8), brightness: u8) -> (u8, u8, u8) {
    let scale =
        |c: u8| u8::try_from(u16::from(c) * u16::from(brightness.min(100)) / 100).unwrap_or(c);
    (scale(color.0), scale(color.1), scale(color.2))
}

fn mix_rgb(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let mix = |x: u8, y: u8| ((f32::from(x) * (1.0 - t)) + (f32::from(y) * t)) as u8;
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

fn fract(value: f32) -> f32 {
    value - value.floor()
}

fn hash(a: u32, b: u32) -> f32 {
    let mut x = a.wrapping_mul(0x9e37_79b9) ^ b.wrapping_mul(0x85eb_ca6b);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    f32::from(u8::try_from(x >> 24).unwrap_or(0)) / 255.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spectrum_pulse_emits_one_colour_per_zone() {
        let inputs = HostInputs {
            screen: None,
            audio_rms: 0.0,
            audio_bands: [0.0; 8],
            press_age_ms: 1_000,
        };
        let zones = frame(
            LightingEffect::SpectrumPulse,
            0.5,
            50,
            (0, 162, 255),
            80,
            &[0, 1],
            false,
            &inputs,
        );
        assert_eq!(zones.len(), 2);
        assert_eq!(zones[0].0, 0);
        assert_eq!(zones[1].0, 1);
    }

    #[test]
    fn echo_press_is_bright_just_after_a_press() {
        let hot = echo_pixel((255, 0, 0), 10);
        let cold = echo_pixel((255, 0, 0), 800);
        assert!(hot.0 > cold.0);
    }
}
