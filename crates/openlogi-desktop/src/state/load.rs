//! View-model projection of background device reads.

use std::sync::Arc;

use openlogi_core::hid::{DpiInfo, OnboardProfilesInfo, SmartShiftStatus};

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
    /// the feature, so re-selecting it grants a fresh attempt.
    Failed(String),
    /// The device genuinely does not support the feature; never retried.
    Unsupported(String),
}

/// Per-device DPI capability load state. See [`Load`].
pub type DpiStatus = Load<Arc<DpiInfo>>;

/// Per-device SmartShift (`0x2111`) config load state. See [`Load`].
pub type SmartShiftLoad = Load<Arc<SmartShiftStatus>>;

/// Per-device onboard-profile (`0x8100`) state load. See [`Load`].
pub type ProfilesLoad = Load<Arc<OnboardProfilesInfo>>;
