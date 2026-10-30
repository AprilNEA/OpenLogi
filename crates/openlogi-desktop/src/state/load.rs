//! View-model projection of background device reads.

use std::sync::Arc;

use openlogi_core::hid::{
    BacklightState, DisableKeysState, DpiInfo, EasySwitchHosts, FnLockState, ForceSensingInfo,
    OnboardState, PointerSpeed, ReportRateInfo, SmartShiftStatus,
};

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

/// Per-mouse report-rate (`0x8060` / `0x8061`) load state. See [`Load`]. The
/// read carries the rate the mouse reports at now and the rates it offers on
/// its current connection, which change when it moves between a receiver and
/// a cable.
pub type ReportRateLoad = Load<Arc<ReportRateInfo>>;
/// Per-mouse pointer-speed (`0x2205`) load state. See [`Load`]. The read is
/// the speed the mouse moves at now: the persisted `pointer_speed`, or the
/// slowest speed while precision mode is on.
pub type PointerSpeedLoad = Load<Arc<PointerSpeed>>;
/// Per-device press-force (`0x19c0`) load state. See [`Load`]. The read is
/// the force the button clicks at now, the range the device accepts, and
/// whether software may change it.
pub type ForceSensingLoad = Load<Arc<ForceSensingInfo>>;
/// Per-keyboard backlight (`0x1982`) load state. See [`Load`]. The keyboard
/// stores its backlight itself and changes it on its own — the backlight keys,
/// the light sensor, a low battery — so the read is the only record.
pub type BacklightLoad = Load<Arc<BacklightState>>;

/// Per-device Easy-Switch (`0x1815`) load state. See [`Load`]. The device
/// stores each slot's host name and OS itself — the computer on the slot
/// writes them — so the read is the only record.
pub type EasySwitchLoad = Load<Arc<EasySwitchHosts>>;

/// Per-device Disable Keys capability/current-state load.
pub type DisableKeysLoad = Load<Arc<DisableKeysState>>;

pub type OnboardLoad = Load<Arc<OnboardState>>;
