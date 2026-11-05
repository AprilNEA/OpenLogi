#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Full,
    Reduced,
}

impl Motion {
    const fn reduce_motion(self) -> bool {
        matches!(self, Self::Reduced)
    }
}

#[derive(Debug, Default)]
pub struct MotionPreference {
    last_known: Option<Motion>,
}

impl MotionPreference {
    #[must_use]
    pub fn resolve(&mut self, read: Option<Motion>) -> bool {
        if let Some(motion) = read {
            self.last_known = Some(motion);
            motion.reduce_motion()
        } else {
            self.last_known.is_some_and(Motion::reduce_motion)
        }
    }
}

#[must_use]
pub fn read_reduced_motion() -> Option<Motion> {
    #[cfg(target_os = "macos")]
    {
        macos::read()
    }
    #[cfg(target_os = "windows")]
    {
        windows::read()
    }
    #[cfg(target_os = "linux")]
    {
        linux::read()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use std::process::Command;
    #[cfg(target_os = "linux")]
    use std::time::{Duration, Instant};

    #[cfg(target_os = "linux")]
    use super::linux::{MOTION_QUERY_TIMEOUT, parse_gsettings_bool, read_bool_command};
    use super::{Motion, MotionPreference};

    #[test]
    fn motion_preference_unknown_defaults_to_motion_on() {
        let mut preference = MotionPreference::default();
        assert!(!preference.resolve(None));
    }

    #[test]
    fn motion_preference_tracks_full_then_reduced_between_opens() {
        let mut preference = MotionPreference::default();
        assert!(!preference.resolve(Some(Motion::Full)));
        assert!(preference.resolve(Some(Motion::Reduced)));
    }

    #[test]
    fn motion_preference_failed_read_keeps_last_known() {
        let mut preference = MotionPreference::default();
        assert!(!preference.resolve(Some(Motion::Full)));
        assert!(!preference.resolve(None));
        assert!(preference.resolve(Some(Motion::Reduced)));
        assert!(preference.resolve(None));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn gsettings_booleans_parse_and_anything_else_is_no_signal() {
        assert_eq!(parse_gsettings_bool("true\n"), Some(true));
        assert_eq!(parse_gsettings_bool("false\n"), Some(false));
        assert_eq!(parse_gsettings_bool(""), None);
        assert_eq!(parse_gsettings_bool("No such schema\n"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_motion_query_returns_its_parsed_answer() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo false"]);
        assert_eq!(
            read_bool_command(command, Duration::from_secs(5)),
            Some(false)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_hung_motion_query_is_killed_at_its_timeout() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5; echo false"]);
        let started = Instant::now();
        assert_eq!(read_bool_command(command, MOTION_QUERY_TIMEOUT), None);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
