//! The lighting wave the agent draws itself: a dimmer band crossing keys lit
//! in the configured colour, painted frame by frame over `0x8081`, with the
//! configured key colours held still over it.
//!
//! A wave runs as one lighting job per route that paints until the route's
//! stop flag is raised. Every new lighting write for the route raises it
//! first, so the job lets go of the route's lighting lock within a frame and
//! the new write takes over; so does the agent's exit, before it hands RGB
//! control back to the firmware.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use openlogi_core::color::Rgb;
use openlogi_core::config::{Lighting, WaveDirection};
use openlogi_hid::DeviceRoute;

/// The time a frame aims to take. A G515 over LIGHTSPEED paints about eleven
/// frames a second, so this mostly bounds a faster link.
pub(crate) const FRAME: Duration = Duration::from_millis(70);

/// The stop flag of the wave running on each route.
static RUNNING: LazyLock<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Stop the wave on `route`, if one runs, and return the flag for a new one.
/// The old wave stops before its next frame.
pub(crate) fn start(route: &DeviceRoute) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let previous = RUNNING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(route.to_string(), Arc::clone(&flag));
    if let Some(previous) = previous {
        previous.store(true, Ordering::Release);
    }
    flag
}

/// Stop the wave on `route`, if one runs.
pub(crate) fn stop(route: &DeviceRoute) {
    if let Some(flag) = RUNNING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&route.to_string())
    {
        flag.store(true, Ordering::Release);
    }
}

/// Stop every wave, as the agent leaves.
pub(crate) fn stop_all() {
    for (_, flag) in RUNNING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .drain()
    {
        flag.store(true, Ordering::Release);
    }
}

/// Forget `flag` as `route`'s wave once that wave has ended, unless a newer
/// one has replaced it.
pub(crate) fn finished(route: &DeviceRoute, flag: &Arc<AtomicBool>) {
    let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
    if running
        .get(&route.to_string())
        .is_some_and(|current| Arc::ptr_eq(current, flag))
    {
        running.remove(&route.to_string());
    }
}

/// A wave's look, resolved from a [`Lighting`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WaveLook {
    /// The keys' colour outside the band, brightness applied.
    color: Rgb,
    /// Keys held at their own colour, brightness applied.
    held: Vec<(u8, Rgb)>,
    /// Seconds the band takes to cross the keyboard.
    period: f32,
    /// The band's width, as a share of the keyboard.
    width: f32,
    /// The share of brightness the band's front keeps.
    floor: f32,
    reverse: bool,
}

/// Brightness steps the band is drawn in: enough for a smooth fade, few
/// enough that a frame stays a handful of requests.
const LEVELS: f32 = 8.;

impl WaveLook {
    /// The wave `lighting` asks for, with `color` and `held` already scaled
    /// by its brightness.
    pub(crate) fn new(lighting: &Lighting, color: Rgb, held: Vec<(u8, Rgb)>) -> Self {
        let period_ms = lighting
            .period_ms
            .unwrap_or_else(|| lighting.effect.default_period_ms())
            .clamp(100, 60_000);
        Self {
            color,
            held,
            period: f32::from(period_ms) / 1000.,
            width: f32::from(lighting.wave.width.clamp(5, 100)) / 100.,
            floor: 1. - f32::from(lighting.wave.depth.min(100)) / 100.,
            reverse: lighting.wave.direction == WaveDirection::RightToLeft,
        }
    }

    /// Every key's colour `elapsed` into the wave. `columns` places the keys
    /// across the keyboard, `0..=1000` from left to right.
    pub(crate) fn frame(&self, columns: &[(u8, u16)], elapsed: Duration) -> Vec<(u8, Rgb)> {
        // The front runs from the leading edge to past the trailing one, so
        // the band enters and leaves whole.
        let front = (elapsed.as_secs_f32() / self.period).fract() * (1. + self.width);
        columns
            .iter()
            .map(|&(usage, column)| {
                let held = self.held.iter().rev().find(|(key, _)| *key == usage);
                let color = held.map_or_else(|| self.color_at(column, front), |&(_, color)| color);
                (usage, color)
            })
            .collect()
    }

    fn color_at(&self, column: u16, front: f32) -> Rgb {
        let x = f32::from(column.min(1000)) / 1000.;
        let x = if self.reverse { 1. - x } else { x };
        // Deepest at the front, recovering over `width` behind it; the keys
        // ahead are untouched.
        let behind = front - x;
        let dip = if (0. ..=self.width).contains(&behind) {
            1. - behind / self.width
        } else {
            0.
        };
        let dip = (dip * (LEVELS - 1.)).round() / (LEVELS - 1.);
        scale(self.color, 1. - (1. - self.floor) * dip)
    }
}

fn scale(color: Rgb, share: f32) -> Rgb {
    let (r, g, b) = color.components();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "share is clamped to 0–1, so the product stays in 0–255"
    )]
    let channel = |value: u8| (f32::from(value) * share.clamp(0., 1.)).round() as u8;
    Rgb::new(channel(r), channel(g), channel(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::config::{LightingEffect, WaveShape};

    fn look(direction: WaveDirection) -> WaveLook {
        let lighting = Lighting {
            effect: LightingEffect::Wave,
            period_ms: Some(2000),
            wave: WaveShape {
                depth: 100,
                width: 50,
                direction,
            },
            ..Lighting::default()
        };
        WaveLook::new(
            &lighting,
            Rgb::new(0, 0xa0, 0xff),
            vec![(0x1a, Rgb::new(0xff, 0, 0xff))],
        )
    }

    const LIT: Rgb = Rgb::new(0, 0xa0, 0xff);

    #[test]
    fn the_band_is_deepest_at_its_front_and_spares_the_keys_ahead() {
        let look = look(WaveDirection::LeftToRight);
        assert_eq!(look.color_at(500, 0.5), Rgb::new(0, 0, 0), "the front");
        assert_eq!(look.color_at(750, 0.5), LIT, "ahead of the front");
        assert_eq!(look.color_at(0, 0.5), LIT, "recovered behind");
        let (_, _, blue) = look.color_at(250, 0.5).components();
        assert!(blue > 0 && blue < 0xff, "half recovered: {blue}");
    }

    #[test]
    fn right_to_left_mirrors_the_board() {
        let look = look(WaveDirection::RightToLeft);
        assert_eq!(look.color_at(500, 0.5), Rgb::new(0, 0, 0));
        assert_eq!(look.color_at(250, 0.5), LIT, "ahead, on the left");
    }

    #[test]
    fn held_keys_keep_their_colour_in_every_frame() {
        let look = look(WaveDirection::LeftToRight);
        let columns = [(0x1a, 100), (0x04, 120)];
        for ms in [0, 400, 900, 1700] {
            let frame = look.frame(&columns, Duration::from_millis(ms));
            assert_eq!(frame[0], (0x1a, Rgb::new(0xff, 0, 0xff)));
            assert_eq!(frame.len(), 2);
        }
    }

    #[test]
    fn a_new_wave_stops_the_one_before_it() {
        // A route no other test touches, since the registry is global.
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xfffe,
        };
        let first = start(&route);
        let second = start(&route);
        assert!(first.load(Ordering::Acquire), "replaced");
        assert!(!second.load(Ordering::Acquire));
        finished(&route, &first);
        stop(&route);
        assert!(second.load(Ordering::Acquire), "stopped");
    }
}
