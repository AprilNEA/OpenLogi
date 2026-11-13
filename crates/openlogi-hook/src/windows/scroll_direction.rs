//! Windows' own mouse-wheel scrolling direction: Settings → Bluetooth &
//! devices → Mouse → Scrolling direction, added in Windows 11 24H2.
//!
//! A wheel event the hook passes through scrolls the way that setting says.
//! An impulse the agent suppresses and replays through `SendInput`
//! (smoothing, non-1x sensitivity) did not: it scrolled the other way while
//! the setting was reversed (#2291). The backend therefore turns the delta it
//! hands the callback with the setting, so the replay scrolls the way the
//! same event passed through would — the contract the macOS and Linux
//! software inversion keeps for its own reversal (`Delivery::run`).
//!
//! Windows documents no API for the setting. Settings stores it per user as
//! the `ReverseMouseWheelDirection` DWORD under `HKCU\Control Panel\Mouse`:
//! `0` is "down motion scrolls down", `1` is "down motion scrolls up".
//! Builds before 24H2 have no such value.

use std::time::{Duration, Instant};

use crate::{MouseEvent, negate_scroll_y};

/// How long one read of the setting is reused.
///
/// The hook reads it on the global input path, ahead of a wheel message's
/// delivery, so a burst of wheel events costs one registry read. A change in
/// Settings applies within this time.
const REFRESH: Duration = Duration::from_secs(1);

/// Which way Windows scrolls for a vertical mouse-wheel rotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ScrollDirection {
    /// "Down motion scrolls down": the default, and the only behaviour
    /// before Windows 11 24H2.
    Standard,
    /// "Down motion scrolls up".
    Reversed,
}

impl From<u32> for ScrollDirection {
    /// Interpret a `ReverseMouseWheelDirection` value. Any non-zero value
    /// reverses, the way a Win32 `BOOL` reads.
    fn from(value: u32) -> Self {
        if value == 0 {
            Self::Standard
        } else {
            Self::Reversed
        }
    }
}

/// The setting as last read, reused for [`REFRESH`].
#[derive(Clone, Copy, Debug, Default)]
struct DirectionCache {
    read: Option<(Instant, ScrollDirection)>,
}

impl DirectionCache {
    /// The cached direction, or a fresh `read()` when the cache is empty or
    /// [`REFRESH`] old.
    fn get(&mut self, now: Instant, read: impl FnOnce() -> ScrollDirection) -> ScrollDirection {
        if let Some((at, direction)) = self.read
            && now.saturating_duration_since(at) < REFRESH
        {
            return direction;
        }
        let direction = read();
        self.read = Some((now, direction));
        direction
    }
}

#[cfg(target_os = "windows")]
thread_local! {
    /// Thread-local like `LAST_POINT` in the hook: `WH_MOUSE_LL` callbacks run
    /// on the thread that installed the hook, so the cache needs no
    /// synchronization on the callback path.
    static CACHE: std::cell::Cell<DirectionCache> =
        const { std::cell::Cell::new(DirectionCache { read: None }) };
}

impl ScrollDirection {
    /// The current user's setting, re-read at most every [`REFRESH`] because
    /// the user can change it while the agent runs.
    #[cfg(target_os = "windows")]
    pub(super) fn current() -> Self {
        CACHE.with(|cell| {
            let mut cache = cell.get();
            let direction = cache.get(Instant::now(), Self::read);
            cell.set(cache);
            direction
        })
    }

    /// Read the setting from the registry.
    ///
    /// A missing value (a build before 24H2, or a user who never changed it)
    /// and a failed read are both [`Self::Standard`]: the replay then keeps
    /// the hook's delta, as it did before this setting existed.
    #[cfg(target_os = "windows")]
    #[expect(
        unsafe_code,
        reason = "the setting is read through the Win32 registry API"
    )]
    fn read() -> Self {
        use windows_sys::Win32::Foundation::ERROR_SUCCESS;
        use windows_sys::Win32::System::Registry::{
            HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW,
        };

        let mut value: u32 = 0;
        // The byte size of `value`, in and out.
        let mut size = u32::BITS / 8;
        // SAFETY: both names are NUL-terminated UTF-16 literals with static
        // lifetime. `RRF_RT_REG_DWORD` restricts the read to a 4-byte DWORD,
        // `value` is a live, writable `u32`, and `size` holds its size in
        // bytes, so the call writes at most `size` bytes into `value`. A null
        // `pdwtype` is documented as "type not requested".
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                windows_sys::w!("Control Panel\\Mouse"),
                windows_sys::w!("ReverseMouseWheelDirection"),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&raw mut value).cast(),
                &raw mut size,
            )
        };
        if status == ERROR_SUCCESS {
            Self::from(value)
        } else {
            Self::Standard
        }
    }

    /// `event` with its vertical scroll turned so that replaying it scrolls
    /// the way the same event passed through does. Every other event, and the
    /// horizontal axis, is returned unchanged.
    pub(super) fn apply(self, event: MouseEvent) -> MouseEvent {
        match self {
            Self::Standard => event,
            Self::Reversed => negate_scroll_y(event),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::ScrollDelta;

    fn scroll(x: f64, y: f64) -> MouseEvent {
        MouseEvent::Scroll {
            delta: ScrollDelta::wheel_ticks(x, y),
            from_trackpad: false,
            device: None,
        }
    }

    #[test]
    fn registry_value_maps_like_a_win32_bool() {
        assert_eq!(ScrollDirection::from(0), ScrollDirection::Standard);
        assert_eq!(ScrollDirection::from(1), ScrollDirection::Reversed);
        assert_eq!(ScrollDirection::from(2), ScrollDirection::Reversed);
    }

    /// #2291: with "down motion scrolls up" on, the agent's replay of a
    /// suppressed wheel impulse must come out reversed, as the pass-through
    /// event does.
    #[test]
    fn reversed_direction_turns_only_the_vertical_wheel_delta() {
        assert_eq!(
            ScrollDirection::Reversed.apply(scroll(0.0, 1.0)),
            scroll(0.0, -1.0)
        );
        assert_eq!(
            ScrollDirection::Reversed.apply(scroll(2.0, -1.0)),
            scroll(2.0, 1.0)
        );
    }

    #[test]
    fn standard_direction_keeps_the_hook_delta() {
        assert_eq!(
            ScrollDirection::Standard.apply(scroll(0.0, 1.0)),
            scroll(0.0, 1.0)
        );
    }

    #[test]
    fn non_scroll_events_are_untouched() {
        let moved = MouseEvent::Moved {
            delta_x: 3,
            delta_y: -4,
        };
        assert_eq!(ScrollDirection::Reversed.apply(moved.clone()), moved);
    }

    /// The hook asks for the direction on every wheel message; the registry is
    /// read once per [`REFRESH`] window, then again so a changed setting lands.
    #[test]
    fn cache_reads_the_setting_once_per_refresh_window() {
        let start = Instant::now();
        let reads = Cell::new(0);
        let read = |direction| {
            reads.set(reads.get() + 1);
            direction
        };
        let mut cache = DirectionCache::default();

        assert_eq!(
            cache.get(start, || read(ScrollDirection::Reversed)),
            ScrollDirection::Reversed
        );
        assert_eq!(
            cache.get(start + Duration::from_millis(999), || {
                read(ScrollDirection::Standard)
            }),
            ScrollDirection::Reversed,
            "a read within REFRESH is reused"
        );
        assert_eq!(reads.get(), 1);

        assert_eq!(
            cache.get(start + REFRESH, || read(ScrollDirection::Standard)),
            ScrollDirection::Standard,
            "a REFRESH-old read is replaced"
        );
        assert_eq!(reads.get(), 2);
    }
}
