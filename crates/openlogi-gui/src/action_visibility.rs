//! Which [`Action`]s are worth offering as a *new* pick on this host, as
//! opposed to which ones execute successfully — every action still executes
//! (or debug-logs a no-op) on every platform per `openlogi_inject::execute`,
//! this only controls what shows up in the picker/Actions Ring catalogs so a
//! desktop-specific action doesn't clutter the list on a host that can't use
//! it. An already-persisted binding is unaffected: it keeps rendering via
//! [`Action::label`] and keeps firing normally, it just can't be picked again
//! from a catalog that hides it.

use openlogi_core::binding::Action;

/// Whether `action` should appear in a pickable catalog on this host.
pub(crate) fn is_offered_here(action: &Action) -> bool {
    match action {
        Action::GnomeOverview => is_gnome_session(),
        _ => true,
    }
}

/// Whether the current session's desktop environment is GNOME (or a
/// GNOME-based session, e.g. GNOME Classic/Flashback).
///
/// Reads `$XDG_CURRENT_DESKTOP`, the same colon-separated variable `.desktop`
/// entries use for `OnlyShowIn=GNOME;` filtering — there is no more
/// authoritative cross-desktop-environment signal than the one the desktop
/// entry spec itself standardized for this exact purpose. Cached: the
/// session's desktop environment cannot change without a re-login, and this
/// is read on every popover render.
#[cfg(target_os = "linux")]
fn is_gnome_session() -> bool {
    static IS_GNOME: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IS_GNOME.get_or_init(|| current_desktop_is_gnome(std::env::var("XDG_CURRENT_DESKTOP").ok()))
}

#[cfg(not(target_os = "linux"))]
fn is_gnome_session() -> bool {
    false
}

/// Pure parse of `$XDG_CURRENT_DESKTOP`'s value, split out from
/// [`is_gnome_session`] so the colon-list/case-insensitivity handling is
/// testable without mutating the process-global environment (racy under a
/// parallel test harness) or fighting the `OnceLock` cache.
#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "linux-only caller")
)]
fn current_desktop_is_gnome(xdg_current_desktop: Option<String>) -> bool {
    xdg_current_desktop.is_some_and(|value| {
        value
            .split(':')
            .any(|part| part.eq_ignore_ascii_case("GNOME"))
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
mod tests {
    use super::*;

    #[test]
    fn non_gated_actions_are_always_offered() {
        assert!(is_offered_here(&Action::LeftClick));
        assert!(is_offered_here(&Action::MissionControl));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn gnome_overview_is_hidden_off_linux() {
        assert!(!is_offered_here(&Action::GnomeOverview));
    }

    #[test]
    fn recognizes_plain_and_prefixed_gnome_desktop() {
        assert!(current_desktop_is_gnome(Some("GNOME".into())));
        assert!(current_desktop_is_gnome(Some("ubuntu:GNOME".into())));
        assert!(current_desktop_is_gnome(Some("GNOME-classic:GNOME".into())));
        assert!(current_desktop_is_gnome(Some("gnome".into())));
    }

    #[test]
    fn rejects_other_or_missing_desktops() {
        assert!(!current_desktop_is_gnome(Some("KDE".into())));
        assert!(!current_desktop_is_gnome(Some("XFCE".into())));
        assert!(!current_desktop_is_gnome(None));
    }
}
