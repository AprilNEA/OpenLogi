//! Lighting the agent draws itself, frame by frame over `0x8081`, with the
//! configured key colours held still over it: the wave (a dimmer band
//! crossing keys lit in the configured colour) and the idle rays (streaks of
//! another colour running along the key rows).
//!
//! Drawn lighting, and any idle lighting, runs as one lighting job per route
//! that keeps going until the route's stop flag is raised. Every new lighting write for the route raises it
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

/// The stop flag of the drawn or idle lighting running on each route.
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

/// The steps of brightness a drawn fade takes: enough to look smooth, few
/// enough that a frame stays a handful of requests.
const LEVELS: f32 = 8.;
/// Rays running at once, each on its own lane.
const RAYS: u32 = 4;
/// A ray's length, as a share of the keyboard's width.
const RAY_LENGTH: f32 = 0.45;
/// Brightness steps a ray's tail fades in.
const RAY_STEPS: f32 = 4.;
/// Rows on the keyboard a ray can run along.
const ROWS: u32 = 6;

/// Something the agent draws frame by frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Drawn {
    Wave(WaveLook),
    Rays(RaysLook),
}

impl Drawn {
    /// Every key's colour `elapsed` into the drawing. `positions` places the
    /// keys: `0..=1000` from left to right, and their row.
    pub(crate) fn frame(&self, positions: &[(u8, u16, u8)], elapsed: Duration) -> Vec<(u8, Rgb)> {
        match self {
            Self::Wave(look) => look.frame(positions, elapsed),
            Self::Rays(look) => look.frame(positions, elapsed),
        }
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

    fn frame(&self, positions: &[(u8, u16, u8)], elapsed: Duration) -> Vec<(u8, Rgb)> {
        // The front runs from the leading edge to past the trailing one, so
        // the band enters and leaves whole.
        let front = (elapsed.as_secs_f32() / self.period).fract() * (1. + self.width);
        positions
            .iter()
            .map(|&(usage, column, _)| {
                let color =
                    held_color(&self.held, usage).unwrap_or_else(|| self.color_at(column, front));
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

/// The idle rays: streaks of `ray` running left to right along the key rows
/// over keys lit in `base`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RaysLook {
    base: Rgb,
    ray: Rgb,
    held: Vec<(u8, Rgb)>,
}

impl RaysLook {
    /// Rays in `ray` over `base`, with `held` keys kept in their colours;
    /// every colour already scaled by brightness.
    pub(crate) fn new(base: Rgb, ray: Rgb, held: Vec<(u8, Rgb)>) -> Self {
        Self { base, ray, held }
    }

    fn frame(&self, positions: &[(u8, u16, u8)], elapsed: Duration) -> Vec<(u8, Rgb)> {
        let heads = ray_heads(elapsed);
        positions
            .iter()
            .map(|&(usage, column, row)| {
                let color = held_color(&self.held, usage).unwrap_or_else(|| {
                    let lit = ray_lit(&heads, column, row);
                    if lit > 0. {
                        mix(self.base, self.ray, lit)
                    } else {
                        self.base
                    }
                });
                (usage, color)
            })
            .collect()
    }
}

/// The colour `usage` is held at, the last entry winning.
fn held_color(held: &[(u8, Rgb)], usage: u8) -> Option<Rgb> {
    held.iter()
        .rev()
        .find(|(key, _)| *key == usage)
        .map(|&(_, color)| color)
}

/// Each ray's row and head position at `elapsed`: every lane crosses at its
/// own pace, and picks another row each time it starts over.
fn ray_heads(elapsed: Duration) -> Vec<(u8, f32)> {
    (0..RAYS)
        .map(|lane| {
            #[expect(clippy::cast_precision_loss, reason = "a lane number is tiny")]
            let lane_f = lane as f32;
            let crossing = 1.6 + 0.5 * lane_f;
            let run = elapsed.as_secs_f32() / crossing + 0.31 * lane_f;
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a run count is non-negative and far below u32::MAX"
            )]
            let round = run.floor() as u32;
            let row = u8::try_from(mix_bits(lane.wrapping_mul(7919) ^ round) % ROWS).unwrap_or(0);
            (row, run.fract() * (1. + RAY_LENGTH))
        })
        .collect()
}

/// How lit a key at `column` on `row` is by the rays: `1` under a head,
/// fading along the tail behind it, `0` away from every ray.
fn ray_lit(heads: &[(u8, f32)], column: u16, row: u8) -> f32 {
    let x = f32::from(column.min(1000)) / 1000.;
    heads
        .iter()
        .filter(|(ray_row, _)| *ray_row == row)
        .map(|&(_, head)| {
            let behind = head - x;
            if (0. ..=RAY_LENGTH).contains(&behind) {
                let lit = 1. - behind / RAY_LENGTH;
                (lit * RAY_STEPS).ceil() / RAY_STEPS
            } else {
                0.
            }
        })
        .fold(0., f32::max)
}

/// A cheap integer hash, so each lane's rows look random without a random
/// number generator.
const fn mix_bits(mut value: u32) -> u32 {
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^ (value >> 16)
}

/// `from` moved `share` of the way to `to`.
fn mix(from: Rgb, to: Rgb, share: f32) -> Rgb {
    let share = share.clamp(0., 1.);
    let (r1, g1, b1) = from.components();
    let (r2, g2, b2) = to.components();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a blend of two 0–255 values stays in 0–255"
    )]
    let channel =
        |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * share).round() as u8;
    Rgb::new(channel(r1, r2), channel(g1, g2), channel(b1, b2))
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

    const LIT: Rgb = Rgb::new(0, 0xa0, 0xff);
    const MAGENTA: Rgb = Rgb::new(0xff, 0, 0xff);
    const WHITE: Rgb = Rgb::new(0xff, 0xff, 0xff);

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
        WaveLook::new(&lighting, LIT, vec![(0x1a, MAGENTA)])
    }

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
        let drawn = [
            Drawn::Wave(look(WaveDirection::LeftToRight)),
            Drawn::Rays(RaysLook::new(LIT, WHITE, vec![(0x1a, MAGENTA)])),
        ];
        let positions = [(0x1a, 100, 2), (0x04, 120, 3)];
        for drawing in &drawn {
            for ms in [0, 400, 900, 1700] {
                let frame = drawing.frame(&positions, Duration::from_millis(ms));
                assert_eq!(frame[0], (0x1a, MAGENTA));
                assert_eq!(frame.len(), 2);
            }
        }
    }

    #[test]
    fn rays_light_only_the_keys_under_them() {
        let rays = RaysLook::new(LIT, WHITE, Vec::new());
        let positions: Vec<(u8, u16, u8)> = (0..6u8)
            .flat_map(|row| (0..20u8).map(move |i| (row * 20 + i, u16::from(i) * 50, row)))
            .collect();
        let at = Duration::from_millis(700);
        let frame = rays.frame(&positions, at);
        let heads = ray_heads(at);
        let mut lit_keys = 0;
        for (&(_, column, row), &(_, color)) in positions.iter().zip(&frame) {
            if ray_lit(&heads, column, row) > 0. {
                lit_keys += 1;
                assert_ne!(color, LIT, "a key under a ray turns towards white");
            } else {
                assert_eq!(color, LIT, "a key away from every ray keeps the base");
            }
        }
        assert!(lit_keys > 0, "some ray is on the board");
    }

    #[test]
    fn a_new_run_stops_the_one_before_it() {
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
