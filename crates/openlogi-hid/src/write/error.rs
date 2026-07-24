use hidpp::protocol::v20::{ErrorType, Hidpp20Error};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Error returned by HID++ read/write operations.
///
/// Serializable + Clone so it can cross the agent↔GUI IPC unchanged: the GUI
/// classifies a device read/write error as permanent (FeatureUnsupported /
/// EmptyDpiList) vs transient, so the discriminating variant must survive the
/// wire — stringifying it would collapse every case to "transient" and a device
/// that genuinely lacks a feature would be re-probed forever. Variant order is
/// therefore wire format: changes require a `PROTOCOL_VERSION` bump (guarded
/// by `openlogi-agent-core/tests/wire_format.rs`).
#[derive(Debug, Clone, Error, Serialize, Deserialize)]
pub enum WriteError {
    /// HID transport error serialized as text.
    ///
    /// `async_hid::HidError` isn't `Serialize`, so carry its message as text;
    /// the typed error is never matched on (only constructed + displayed).
    #[error("HID transport error: {0}")]
    Hid(String),
    /// No currently connected device matched the requested route.
    #[error("no connected device matched the route")]
    DeviceNotFound,
    /// The HID node opened, but the target HID++ device index did not answer.
    #[error("device at index {index:#04x} did not respond to HID++")]
    DeviceUnreachable {
        /// HID++ device index that failed to answer.
        index: u8,
    },
    /// Device does not expose the requested HID++ feature.
    #[error("device does not expose HID++ feature {feature_hex:#06x}")]
    FeatureUnsupported {
        /// HID++ feature ID that was not present.
        feature_hex: u16,
    },
    /// Device reported no valid DPI values.
    #[error("device returned no supported DPI values")]
    EmptyDpiList,
    /// Generic HID++ protocol error serialized as text.
    #[error("HID++ protocol error: {0}")]
    Hidpp(String),
    /// HID++ feature error response.
    #[error("HID++ feature error during {operation:?} for feature {feature_hex:#06x}: {kind:?}")]
    HidppFeature {
        /// Operation being performed.
        operation: HidppOperation,
        /// HID++ feature ID involved in the operation.
        feature_hex: u16,
        /// HID++ feature error kind.
        kind: HidppFeatureErrorKind,
    },
    /// Device returned a structurally unsupported response.
    #[error("HID++ unsupported response during {operation:?} for feature {feature_hex:#06x}")]
    UnsupportedResponse {
        /// Operation being performed.
        operation: HidppOperation,
        /// HID++ feature ID involved in the operation.
        feature_hex: u16,
    },
    /// HID++ request timed out.
    #[error("HID++ request timed out during {operation:?}")]
    RequestTimedOut {
        /// Operation that timed out.
        operation: HidppOperation,
    },
    /// Tokio runtime could not be initialized for a sync caller.
    #[error("tokio runtime init failed: {message}")]
    RuntimeInit {
        /// Runtime initialization error message.
        message: String,
    },
    /// Background agent write path is unavailable.
    #[error("background agent is unavailable")]
    AgentUnavailable,
}

/// HID++ operation being performed when a device write/read failed.
///
/// Variant order is wire format because this travels inside [`WriteError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HidppOperation {
    /// Resolve a feature ID to its runtime feature index.
    ResolveFeature,
    /// Enumerate the device feature table.
    DumpFeatures,
    /// Read current DPI.
    ReadDpi,
    /// Read DPI capabilities.
    ReadDpiCapabilities,
    /// Write DPI.
    WriteDpi,
    /// Read SmartShift status.
    ReadSmartShift,
    /// Write SmartShift status.
    WriteSmartShift,
    /// Write keyboard lighting.
    Lighting,
    /// Read HiResWheel capabilities or the current wheel mode.
    ReadWheelMode,
    /// Write and verify the native HiResWheel mode.
    WriteWheelMode,
    /// Read onboard-profiles state (description, mode, active profile,
    /// directory).
    ReadOnboardProfiles,
    /// Write the onboard mode or the active onboard profile.
    WriteOnboardProfiles,
}

/// HID++ feature error kind in a serializable wire-safe form.
///
/// Variant order is wire format because this travels inside [`WriteError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HidppFeatureErrorKind {
    /// HID++ `NoError` code.
    NoError,
    /// Unknown HID++ error.
    Unknown,
    /// Invalid argument.
    InvalidArgument,
    /// Argument out of range.
    OutOfRange,
    /// Hardware error.
    HwError,
    /// Logitech-internal firmware error.
    LogitechInternal,
    /// Invalid feature index.
    InvalidFeatureIndex,
    /// Invalid function ID.
    InvalidFunctionId,
    /// Device is busy.
    Busy,
    /// Operation is unsupported.
    Unsupported,
    /// Error code not modeled by OpenLogi.
    Unrecognized,
}

impl From<async_hid::HidError> for WriteError {
    fn from(e: async_hid::HidError) -> Self {
        Self::Hid(e.to_string())
    }
}

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
