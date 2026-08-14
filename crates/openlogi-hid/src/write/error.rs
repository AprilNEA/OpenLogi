use hidpp::protocol::v20::{ErrorType, Hidpp20Error};

// WriteError, HidppOperation, and HidppFeatureErrorKind are pure IPC wire
// data with no HID++/async-hid I/O, so they live in
// `openlogi_core::hid::error`; re-exported here unchanged so this module's
// own API surface doesn't churn. The conversions below stay here because
// they name `hidpp`/`async_hid` types, which `openlogi-core` must never
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

/// Converts an `async_hid` transport error into [`WriteError`].
///
/// `async_hid::HidError` isn't `Serialize`, so its message is carried as
/// text; the typed error is never matched on (only constructed + displayed).
/// A plain function rather than a `From` impl: with `WriteError` now defined
/// in `openlogi-core`, `impl From<async_hid::HidError> for WriteError` here
/// would implement a foreign trait for a foreign type and violate the
/// orphan rule.
pub(crate) fn hid_error_to_write_error(error: &async_hid::HidError) -> WriteError {
    WriteError::Hid(error.to_string())
}
