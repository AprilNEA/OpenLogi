//! `resolve_dpi`'s precedence — a live read beats the persisted config value,
//! which beats the hardcoded default — plus the `AppState`-level wiring that
//! feeds it the configured value.

use super::*;
use crate::state::{DEFAULT_DPI, DpiLoad};

fn ready(dpi: Dpi) -> DpiLoad {
    Load::Ready(std::sync::Arc::new(DpiInfo {
        current: dpi,
        capabilities: DpiCapabilities::new(vec![dpi.into_inner()]).expect("valid DPI list"),
    }))
}

#[test]
fn a_ready_live_read_wins_over_a_different_configured_value() {
    assert_eq!(
        resolve_dpi(Some(&ready(Dpi::new(1200))), Some(Dpi::new(400))),
        Dpi::new(1200),
        "the sensor's real value must win — firmware can clamp or reject a \
         requested DPI, so the live read is the only source of truth once \
         it lands"
    );
}

#[test]
fn the_configured_value_is_used_while_the_live_read_is_pending() {
    for pending in [Load::Unknown, Load::Loading] {
        assert_eq!(
            resolve_dpi(Some(&pending), Some(Dpi::new(400))),
            Dpi::new(400)
        );
    }
    assert_eq!(resolve_dpi(None, Some(Dpi::new(400))), Dpi::new(400));
}

#[test]
fn the_configured_value_is_used_when_the_live_read_failed() {
    let failed = Load::Failed("device unreachable".to_string());
    assert_eq!(
        resolve_dpi(Some(&failed), Some(Dpi::new(400))),
        Dpi::new(400)
    );
}

#[test]
fn the_hardcoded_default_is_used_with_neither_a_live_read_nor_a_configured_value() {
    assert_eq!(resolve_dpi(None, None), DEFAULT_DPI);
    assert_eq!(resolve_dpi(Some(&Load::Unknown), None), DEFAULT_DPI);
}

#[test]
fn the_persisted_config_value_is_shown_before_the_live_read_resolves() {
    let mut state = state_with_a_known_mouse();
    state
        .config
        .edit(|config| config.set_dpi(KNOWN_MOUSE_KEY, Dpi::new(400)));

    assert_eq!(
        state.dpi_for_current(),
        Dpi::new(400),
        "the value the user configured must be shown while the agent's live \
         DPI read is still pending, not an unrelated hardcoded default"
    );
}

#[test]
fn the_hardcoded_default_is_shown_with_neither_a_live_read_nor_a_configured_value() {
    let state = state_with_a_known_mouse();

    assert_eq!(state.dpi_for_current(), DEFAULT_DPI);
}
