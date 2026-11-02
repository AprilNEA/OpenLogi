//! Typed session idle time, independent of the native `GetLastInputInfo` read.

use std::time::Duration;

/// A `GetTickCount` reading: milliseconds since boot in a `u32` that wraps
/// about every 49.7 days. `LASTINPUTINFO::dwTime` is stamped on the same clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TickCount(pub(super) u32);

impl TickCount {
    /// Time from `earlier` to `self`, correct across one wrap of the counter.
    pub(super) fn duration_since(self, earlier: Self) -> Duration {
        Duration::from_millis(u64::from(self.0.wrapping_sub(earlier.0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_time_is_the_tick_difference() {
        assert_eq!(
            TickCount(90_000).duration_since(TickCount(62_516)),
            Duration::from_millis(27_484)
        );
    }

    #[test]
    fn idle_time_survives_the_tick_counter_wrapping() {
        assert_eq!(
            TickCount(1_000).duration_since(TickCount(u32::MAX - 999)),
            Duration::from_millis(2_000)
        );
    }
}
