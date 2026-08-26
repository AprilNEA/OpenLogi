//! One-shot user notifications from the agent.
//!
//! The agent is the always-on process, so it raises these itself rather than
//! routing through the on-demand GUI — a GUI that is closed (the normal state)
//! could not deliver an alert at the moment it matters, and cold-launching a
//! window just to show a toast is worse than the toast.
//!
//! Strings arrive already formatted and in English; the agent links no i18n.

/// Show a notification. Fire-and-forget: one that cannot be shown is logged,
/// never fatal — the agent's real work must not depend on a shell surface.
pub fn notify(title: &str, body: &str) {
    #[cfg(target_os = "windows")]
    crate::tray_windows::notify(title, body);

    #[cfg(not(target_os = "windows"))]
    {
        // macOS gets `UNUserNotificationCenter` in M2; Linux has no tray and
        // no notification surface in this app at all. Logged rather than
        // silently dropped so the alert path is still observable there.
        tracing::debug!(
            title,
            body,
            "notification requested on a platform that cannot show one yet"
        );
    }
}
