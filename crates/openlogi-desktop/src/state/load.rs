//! View-model projection of background device reads.

use std::sync::Arc;

use openlogi_core::hid::{DisableKeysState, DpiInfo, FnLockState, SmartShiftStatus};

/// State projected from an swr-backed device query: unqueried, in flight,
/// resolved, transiently failed, or permanently unsupported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Load<T> {
    /// The selected device has not been queried yet. Also what a device
    /// nobody has asked about reads as.
    #[default]
    Unknown,
    /// A background HID++ read is in flight.
    Loading,
    /// The device reported its value.
    Ready(T),
    /// Transient errors (read timeouts, busy device) exhausted the retry budget.
    /// Distinct from [`Self::Unsupported`] because the device may well support
    /// the feature — re-selecting it grants a fresh attempt.
    Failed(String),
    /// The device genuinely does not support the feature; never retried.
    Unsupported(String),
}

/// Per-device DPI capability load state. See [`Load`].
pub type DpiLoad = Load<Arc<DpiInfo>>;

/// Per-device SmartShift (`0x2111`) config load state. See [`Load`]. Unlike DPI
/// presets, the resolved config is *not* persisted to `config.toml` — the device
/// stores wheel mode / threshold / torque in its own non-volatile memory, so the
/// GUI only ever reads and writes the device.
pub type SmartShiftLoad = Load<Arc<SmartShiftStatus>>;

/// Per-keyboard Fn-lock (`0x40a2` / `0x40a3`) load state. See [`Load`]. The
/// read shows what the keyboard holds right now — it can differ from the
/// persisted `fn_lock` after the user pressed Fn+Esc on the keyboard.
pub type FnLockLoad = Load<Arc<FnLockState>>;
/// Per-device Disable Keys capability/current-state load.
pub type DisableKeysLoad = Load<Arc<DisableKeysState>>;
