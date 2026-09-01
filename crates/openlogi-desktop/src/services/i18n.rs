//! Settings-app localization.
//!
//! Translations live in `crates/openlogi-ui/locales/*.toml` and are loaded at
//! compile time by the `rust_i18n::i18n!` macro in `main.rs` (fallback `"en"`).
//! **`en.toml` is the English source of truth**. Stable semantic keys are
//! shared by every locale, so English copy can change without changing message
//! identity. New messages must land in **every** `locales/*.toml` in the same
//! change — `openlogi-ui`'s parity test enforces a key-for-key match. Crowdin
//! improves non-English values over time and the workflow downloads only real
//! translations (`skip_untranslated_strings`). Call sites use
//! [`tr!`](crate::tr) / `rust_i18n::t!` with product-domain keys such as
//! `device.connected`. Missing keys render as the key, so catalogs must not lag.
//!
//! The current locale is a process-global atomic inside `rust_i18n`. Setting it
//! re-localizes both our own call sites *and* gpui-component's built-in widget
//! strings, since the framework reads the same global. Apply it once at startup
//! via [`apply`] and on a live switch via
//! [`AppState::set_language`](crate::state::AppState::set_language); each must be
//! followed by a window refresh so open views re-render with the new locale.
//!
//! Which catalog a BCP-47 code resolves to is decided in
//! [`openlogi_core::locale`], shared with the overlay helper.

use openlogi_core::config::AppSettings;
use openlogi_core::locale::activate;

/// Serializes tests that mutate `rust_i18n`'s process-global locale, so a
/// locale switch in one test cannot interleave with another's assertions.
#[cfg(test)]
pub(crate) static LOCALE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Apply the configured language to the process-global locale at startup.
/// Safe to call before any window opens — the locale is a plain atomic.
pub fn apply(settings: &AppSettings) {
    activate(settings.language.as_deref());
}

#[cfg(test)]
mod tests;
