//! HID++ read/write error vocabulary — pure data, no I/O.
//!
//! The conversions from `hidpp`/`async_hid` error types into [`WriteError`]
//! live in `openlogi_hid::write::error`, which this crate must never depend
//! on.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::backlight::BacklightChange;
use super::dpi::Dpi;
use super::report_rate::ReportRate;

/// Error returned by HID++ read/write operations.
///
/// Serializable + Clone so it can cross the agent↔GUI IPC unchanged: the GUI
/// classifies a device read/write error as permanent (FeatureUnsupported /
/// EmptyDpiList) vs transient, so the discriminating variant must survive the
/// wire — stringifying it would collapse every case to "transient" and a device
/// that genuinely lacks a feature would be re-probed forever. Variant order is
/// therefore wire format: changes require a `PROTOCOL_VERSION` bump (guarded
/// by `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, Error, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriteError {
    /// HID transport error serialized as text.
    ///
    /// `openlogi_hid::BackendError` isn't `Serialize`, so carry its message as
    /// text; the typed error is never matched on (only constructed +
    /// displayed).
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
    /// A standalone light value is outside the driver's supported range.
    #[error("invalid light value for {control}: {value}")]
    InvalidLightValue {
        /// Semantic control name.
        control: String,
        /// Rejected value in the semantic/native unit supplied by the caller.
        value: u16,
    },
    /// The selected light driver does not implement a requested control.
    #[error("light control is unsupported: {control}")]
    LightUnsupported {
        /// Semantic control name.
        control: String,
    },
    /// Multiple raw HID nodes matched one physical route.
    #[error("multiple raw HID devices matched the route")]
    AmbiguousRawDevice,
    /// A DPI write asked for a value the device does not report as supported.
    ///
    /// Nothing was written: the device layer checks the device-reported list
    /// first rather than leaving the firmware to clamp, ignore or reject it.
    #[error("DPI {requested} is not one of the device-reported supported values")]
    DpiUnsupported {
        /// The rejected DPI.
        requested: Dpi,
    },
    /// The device acknowledged a DPI write but reads back a different value,
    /// including after one re-write.
    #[error("device reports DPI {actual} after a write of {requested}")]
    DpiNotApplied {
        /// The DPI that was written.
        requested: Dpi,
        /// The DPI the device reports afterwards.
        actual: Dpi,
    },
    /// A haptic play asked for a waveform the device does not advertise.
    ///
    /// Nothing was sent: the device layer checks the `0x19b0` capability mask
    /// first, as Logi Options+ does, so a retry cannot succeed.
    #[error("device does not advertise haptic waveform {waveform}")]
    HapticWaveformUnsupported {
        /// The waveform ID that was refused.
        waveform: u8,
    },
    /// A report-rate write asked for a rate the device does not list for its
    /// current connection.
    ///
    /// Nothing was written: the device layer checks the device-reported list
    /// first, so the firmware never has to clamp or reject it.
    #[error("report rate {requested} Hz is not one the device lists for this connection")]
    ReportRateUnsupported {
        /// The rejected rate.
        requested: ReportRate,
    },
    /// The device acknowledged a report-rate write but reads back a
    /// different rate — for instance a gaming mouse whose onboard profile
    /// owns the rate.
    #[error("device reports {actual} Hz after a write of {requested} Hz")]
    ReportRateNotApplied {
        /// The rate that was written.
        requested: ReportRate,
        /// The rate the device reports afterwards.
        actual: ReportRate,
    },
    /// A backlight change the keyboard does not offer — a level beyond its
    /// level count, or a mode, level or battery-saving control its `0x1982`
    /// capability bits leave out.
    ///
    /// Nothing was written: the device layer checks a fresh read first.
    #[error("the keyboard does not offer {change}")]
    BacklightChangeUnsupported {
        /// The rejected change.
        change: BacklightChange,
    },
    /// The keyboard acknowledged a backlight write but reads back a
    /// configuration without the change.
    #[error("keyboard does not report {change} after the write")]
    BacklightNotApplied {
        /// The change that was written.
        change: BacklightChange,
    },
    /// A haptic play refused because the device reports a non-zero `0x19b0`
    /// haptic status.
    ///
    /// Nothing was sent: Logi Options+ plays only while the status is zero,
    /// and the device layer applies the same gate, so a retry cannot succeed
    /// until the device reports otherwise.
    #[error("device reports haptic status {status}, so haptics cannot play")]
    HapticsUnavailable {
        /// The raw status byte the device reported.
        status: u8,
    },
    /// A rename of an Easy-Switch slot's computer on a device whose `0x1815`
    /// capabilities leave renaming out.
    ///
    /// Nothing was written.
    #[error("the device does not let software rename its Easy-Switch hosts")]
    HostRenameUnsupported,
    /// A rename of an Easy-Switch slot that has no computer paired, or that
    /// the device does not have.
    ///
    /// Nothing was written.
    #[error("Easy-Switch channel {} has no paired computer", u16::from(*slot) + 1)]
    HostSlotEmpty {
        /// The zero-based slot index asked for.
        slot: u8,
    },
    /// A host name that is empty, holds a NUL, or is longer than the slot
    /// stores.
    ///
    /// Nothing was written.
    #[error("a host name must be 1 to {max_len} bytes of UTF-8 without NUL")]
    HostNameRejected {
        /// The longest name, in UTF-8 bytes, the slot stores.
        max_len: u8,
    },
    /// The device acknowledged a host-name write but reads back another
    /// name.
    #[error("device reports host name {actual:?} after a write of {requested:?}")]
    HostNameNotApplied {
        /// The name that was written.
        requested: String,
        /// The name the device reports afterwards, if any.
        actual: Option<String>,
    },
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
    /// Read the keyboard backlight config or info.
    ReadBacklight,
    /// Write the keyboard backlight config.
    WriteBacklight,
    /// Write keyboard Fn-lock (fn inversion).
    WriteFnLock,
    /// Write a standalone-light command. Appended last — variant order is wire format.
    Light,
    /// Play one haptic waveform. Appended last — variant order is wire format.
    PlayHaptic,
    /// Read keyboard Fn-lock (fn inversion). Appended last — variant order is
    /// wire format.
    ReadFnLock,
    /// Read the `0x2205` pointer-motion scaling. Appended last — variant order
    /// is wire format.
    ReadPointerScaling,
    /// Write the `0x2205` pointer-motion scaling. Appended last — variant order
    /// is wire format.
    WritePointerScaling,
    /// Keep a keyboard's `0x4531` platform on the host OS: read the platform
    /// table and the current host's platform, and write it when it differs.
    /// Appended last — variant order is wire format.
    WriteOsLayout,
    /// Read the `0x8060` / `0x8061` report rate and the rates the device
    /// lists. Appended last — variant order is wire format.
    ReadReportRate,
    /// Write the `0x8060` / `0x8061` report rate. Appended last — variant
    /// order is wire format.
    WriteReportRate,
    /// Read the `0x1815` Easy-Switch host slots, their names and OS.
    /// Appended last — variant order is wire format.
    ReadHosts,
    /// Write an Easy-Switch slot's `0x1815` host name. Appended last —
    /// variant order is wire format.
    RenameHost,
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
