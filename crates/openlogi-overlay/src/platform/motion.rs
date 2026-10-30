//! The OS reduce-motion preference, read before each ring opens.
//!
//! One reader per OS behind this facade: macOS asks `NSWorkspace`, Windows
//! `SystemParametersInfoW`, Linux GNOME's `gsettings`. Each platform file
//! implements the same `read`; the facade owns the setting itself and the
//! cache that stands in for a read that failed. Which invocation opens once
//! the read is in is ring-session sequencing, and lives in
//! [`crate::session::settle_latest_invocation`].

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// What the OS asks of motion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MotionSetting {
    /// Animate as designed.
    Full,
    /// The user asked for less motion: open, close and hover without
    /// animation.
    Reduced,
}

impl MotionSetting {
    /// The setting behind an OS switch that is on when animations play.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    pub(super) const fn from_animations(enabled: bool) -> Self {
        if enabled { Self::Full } else { Self::Reduced }
    }

    /// GPUI's `reduce_motion` flag for this setting.
    #[must_use]
    pub(crate) const fn reduces(self) -> bool {
        matches!(self, Self::Reduced)
    }
}

/// Read the host OS reduce-motion preference once: `None` when it could not
/// be read.
///
/// On Linux this may block the calling thread for up to the query's timeout;
/// call it from a background thread, as
/// [`crate::session::settle_latest_invocation`] does.
#[must_use]
pub(crate) fn read() -> Option<MotionSetting> {
    #[cfg(target_os = "linux")]
    {
        linux::read()
    }
    #[cfg(target_os = "macos")]
    {
        macos::read()
    }
    #[cfg(target_os = "windows")]
    {
        windows::read()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

/// The reads across ring opens: the last one that succeeded stands in for
/// one that fails.
#[derive(Debug, Default)]
pub(crate) struct MotionPreference {
    last_known: Option<MotionSetting>,
}

impl MotionPreference {
    /// Merge a fresh OS read into the cache and return the setting to apply.
    ///
    /// A successful read wins and is remembered. When the read fails or times
    /// out, the last known setting is used; with no prior value, motion stays
    /// full.
    #[must_use]
    pub(crate) fn resolve(&mut self, read: Option<MotionSetting>) -> MotionSetting {
        if let Some(setting) = read {
            self.last_known = Some(setting);
            setting
        } else {
            self.last_known.unwrap_or(MotionSetting::Full)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MotionPreference, MotionSetting};

    #[test]
    fn an_unknown_preference_keeps_motion_full() {
        let mut preference = MotionPreference::default();
        assert_eq!(preference.resolve(None), MotionSetting::Full);
    }

    #[test]
    fn the_preference_tracks_full_then_reduced_between_opens() {
        let mut preference = MotionPreference::default();
        assert_eq!(
            preference.resolve(Some(MotionSetting::Full)),
            MotionSetting::Full
        );
        assert_eq!(
            preference.resolve(Some(MotionSetting::Reduced)),
            MotionSetting::Reduced
        );
    }

    #[test]
    fn a_failed_read_keeps_the_last_known_setting() {
        let mut preference = MotionPreference::default();
        assert_eq!(
            preference.resolve(Some(MotionSetting::Full)),
            MotionSetting::Full
        );
        assert_eq!(preference.resolve(None), MotionSetting::Full);
        assert_eq!(
            preference.resolve(Some(MotionSetting::Reduced)),
            MotionSetting::Reduced
        );
        assert_eq!(preference.resolve(None), MotionSetting::Reduced);
    }
}
