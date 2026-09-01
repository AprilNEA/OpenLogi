//! View-model projection of background device reads.

use std::sync::Arc;

use openlogi_core::hid::{DpiInfo, PowerModeState, SmartShiftStatus};

/// State projected from an swr-backed device query: unqueried, in flight,
/// resolved, transiently failed, or permanently unsupported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Load<T> {
    /// The selected device has not been queried yet.
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
pub type DpiStatus = Load<Arc<DpiInfo>>;

/// Per-device SmartShift (`0x2111`) config load state. See [`Load`]. Unlike DPI
/// presets, the resolved config is *not* persisted to `config.toml` — the device
/// stores wheel mode / threshold / torque in its own non-volatile memory, so the
/// GUI only ever reads and writes the device.
pub type SmartShiftLoad = Load<Arc<SmartShiftStatus>>;

/// Per-device power-mode (`0x8090`) load state. See [`Load`]. The value is
/// never persisted host-side: the device keeps the mode across power cycles
/// itself, so the GUI only ever reads and writes the device.
pub type PowerModeLoad = Load<Arc<PowerModeState>>;
