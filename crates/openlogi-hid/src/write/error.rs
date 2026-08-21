use hidpp::protocol::v20::{ErrorType, Hidpp20Error};

use crate::backend::BackendError;

// WriteError, HidppOperation, and HidppFeatureErrorKind are pure IPC wire
// data with no HID++/backend I/O, so they live in
// `openlogi_core::hid::error`; re-exported here unchanged so this module's
// own API surface doesn't churn. The conversions below stay here because
// they name `hidpp` and backend types, which `openlogi-core` must never
// depend on.
pub use openlogi_core::hid::{HidppFeatureErrorKind, HidppOperation, WriteError};

fn hidpp_feature_error_kind(kind: ErrorType) -> HidppFeatureErrorKind {
    match kind {
        ErrorType::NoError => HidppFeatureErrorKind::NoError,
        ErrorType::Unknown => HidppFeatureErrorKind::Unknown,
        ErrorType::InvalidArgument => HidppFeatureErrorKind::InvalidArgument,
        ErrorType::OutOfRange => HidppFeatureErrorKind::OutOfRange,
        ErrorType::HwError => HidppFeatureErrorKind::HwError,
        ErrorType::LogitechInternal => HidppFeatureErrorKind::LogitechInternal,
        ErrorType::InvalidFeatureIndex => HidppFeatureErrorKind::InvalidFeatureIndex,
        ErrorType::InvalidFunctionId => HidppFeatureErrorKind::InvalidFunctionId,
        ErrorType::Busy => HidppFeatureErrorKind::Busy,
        ErrorType::Unsupported => HidppFeatureErrorKind::Unsupported,
        _ => HidppFeatureErrorKind::Unrecognized,
    }
}

pub(crate) fn classify_hidpp_error(
    error: Hidpp20Error,
    operation: HidppOperation,
    feature_hex: u16,
) -> WriteError {
    match error {
        Hidpp20Error::Feature(kind) => WriteError::HidppFeature {
            operation,
            feature_hex,
            kind: hidpp_feature_error_kind(kind),
        },
        Hidpp20Error::UnsupportedResponse => WriteError::UnsupportedResponse {
            operation,
            feature_hex,
        },
        Hidpp20Error::Channel(error) => WriteError::Hidpp(format!("{error:?}")),
        _ => WriteError::Hidpp(format!("{error:?}")),
    }
}

/// Carries a backend failure across the IPC boundary as text.
///
/// [`WriteError`] is `Serialize` and [`BackendError`] is not, so the message
/// is the payload; the typed error is never matched on downstream. This is a
/// `From` impl rather than a named helper because [`BackendError`] is local to
/// this crate — the orphan rule only blocked it while the source type was
/// `async_hid::HidError`, foreign like [`WriteError`] itself.
impl From<BackendError> for WriteError {
    fn from(error: BackendError) -> Self {
        Self::Hid(error.to_string())
    }
}
