//! Strict `0x6100` frame assembly, physical-unit normalization, and durable
//! raw-report mode ownership.

use std::time::{Duration, Instant};

use hidpp::feature::touchpad_raw_xy::{
    DualXyData, Origin, RawReportFlags, TouchPoint, TouchpadInfo, TouchpadRawXyFeature,
};
use openlogi_core::touchpad::{TouchContact, TouchFrame};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(test)]
mod tests;

/// Fallback silence window before enough complete frames exist to estimate
/// this device's report cadence.
const FALLBACK_STROKE_END_TIMEOUT_US: u64 = 32_000;
/// Four missed complete-frame periods ends a stroke; the bounds tolerate both
/// burst jitter and devices whose cadence differs from Casa Touch's ~130 Hz.
const MISSED_FRAME_PERIODS: u64 = 4;
const MIN_STROKE_END_TIMEOUT_US: u64 = 20_000;
const MAX_STROKE_END_TIMEOUT_US: u64 = 60_000;

/// Requested `0x6100` reporting mode: raw DualXY reports in the 14-bit
/// layout our decoder parses, plus `DIVERT_NATIVE_GESTURES` — the flag the
/// production agent arms (0xC1) so the firmware's whole native-gesture
/// layer stands down for the host while raw reporting streams.
pub const OPENLOGI_RAW_REPORT_FLAGS: RawReportFlags = RawReportFlags::RAW
    .union(RawReportFlags::WIDTH_HEIGHT_8BIT)
    .union(RawReportFlags::DIVERT_NATIVE_GESTURES);

/// The bitmap to arm, honouring the bit-walk probing override: set
/// `OPENLOGI_RAW_FLAGS_OVERRIDE` (hex, e.g. `41`) to test a firmware's
/// behaviour per flag combination without a rebuild.
fn requested_raw_report_flags() -> RawReportFlags {
    let Ok(value) = std::env::var("OPENLOGI_RAW_FLAGS_OVERRIDE") else {
        return OPENLOGI_RAW_REPORT_FLAGS;
    };
    let trimmed = value.trim_start_matches("0x");
    match u8::from_str_radix(trimmed, 16) {
        Ok(bits) => {
            tracing::warn!(bits, "raw-report flags overridden for firmware probing");
            RawReportFlags::from_bits_retain(bits)
        }
        Err(error) => {
            tracing::warn!(%value, %error, "invalid OPENLOGI_RAW_FLAGS_OVERRIDE — using the built-in flags");
            OPENLOGI_RAW_REPORT_FLAGS
        }
    }
}

/// One normalized event emitted by the touchpad stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TouchpadStreamEvent {
    /// One complete frame.
    Frame(TouchFrame),
    /// The previous contact set ended by an empty frame, report silence, or an
    /// abnormal device-timestamp gap.
    End,
    /// Unsupported contact count or malformed data cancelled the stroke.
    Cancel,
    /// Invalid or incomplete logical frames dropped since the prior event.
    DroppedFrames(u64),
}

/// Invalid touchpad geometry or report metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum TouchpadStreamError {
    /// Native DPI is required to convert device coordinates to micrometres.
    #[error("touchpad reported zero native DPI")]
    ZeroDpi,
    /// Timestamp units are required to produce a monotonic microsecond clock.
    #[error("touchpad reported zero timestamp units")]
    ZeroTimestampUnits,
    /// Coordinate dimensions must both be non-zero.
    #[error("touchpad reported an empty coordinate range")]
    EmptyCoordinateRange,
    /// OpenLogi only decodes the observed default DualXY mapping.
    #[error("unsupported touchpad raw-report mapping version {0}")]
    UnsupportedMapping(u8),
}

/// Complete raw-touchpad stream state for one armed capture session.
pub struct TouchpadFrameStream {
    assembler: FrameAssembler,
    active_contacts: Option<usize>,
    last_frame_at: Option<Instant>,
    last_timestamp_us: Option<u64>,
    /// Smoothed complete-frame cadence for the current device, in microseconds.
    cadence_us: Option<u64>,
}

impl TouchpadFrameStream {
    /// Build a stream from the device-reported touchpad characteristics.
    pub fn new(info: TouchpadInfo) -> Result<Self, TouchpadStreamError> {
        Ok(Self {
            assembler: FrameAssembler::new(info)?,
            active_contacts: None,
            last_frame_at: None,
            last_timestamp_us: None,
            cadence_us: None,
        })
    }

    /// Consume one raw DualXY report and return zero or more normalized events.
    ///
    /// Finger-count and contact-ID changes remain in the same stroke so the
    /// recognizer can cancel them rather than reinterpret trailing contacts as
    /// a new gesture.
    pub fn push(&mut self, report: DualXyData, now: Instant) -> Vec<TouchpadStreamEvent> {
        self.push_chunk(report.into(), now)
    }

    fn push_chunk(&mut self, report: RawChunk, now: Instant) -> Vec<TouchpadStreamEvent> {
        let reported_contacts = usize::from(report.finger_count);
        if self.active_contacts.is_some() || reported_contacts != 0 {
            self.last_frame_at = Some(now);
        }
        let outcome = self.assembler.push(report);
        let dropped = self.assembler.take_dropped_frames();
        let mut events = Vec::new();
        if dropped != 0 {
            events.push(TouchpadStreamEvent::DroppedFrames(dropped));
            // The stroke survives a dropped frame: liftoff drops are routine
            // on >2-contact strokes, and the recognizer rebases on the next
            // frame it sees.
            if self.active_contacts.is_some() || reported_contacts != 0 {
                if self.active_contacts.is_none() {
                    self.active_contacts = Some(reported_contacts);
                }
                self.last_frame_at = Some(now);
            }
        }
        let Some(outcome) = outcome else {
            return events;
        };
        match outcome {
            FrameOutcome::Frame(frame) if frame.contacts().is_empty() => {
                events.extend(self.end_event());
                events
            }
            FrameOutcome::Frame(frame) => {
                self.publish_frame(frame, now, &mut events);
                events
            }
            FrameOutcome::Cancel {
                timestamp_us,
                finger_count,
            } => {
                if self.last_timestamp_us.is_some_and(|previous| {
                    timestamp_us.saturating_sub(previous) >= self.stroke_end_timeout_us()
                }) {
                    events.extend(self.end_event());
                }
                self.observe_cadence(timestamp_us);
                self.active_contacts = Some(usize::from(finger_count));
                self.last_frame_at = Some(now);
                events.push(TouchpadStreamEvent::Cancel);
                events
            }
        }
    }

    /// End an active stroke after report silence, dropping an incomplete
    /// final frame first — without cancelling, so the tap is still evaluated.
    pub fn poll_end(&mut self, now: Instant) -> Vec<TouchpadStreamEvent> {
        let Some(last) = self.last_frame_at else {
            return Vec::new();
        };
        if now.saturating_duration_since(last) < self.stroke_end_timeout() {
            return Vec::new();
        }

        let mut events = Vec::new();
        if let Some(finger_count) = self.assembler.drop_pending() {
            events.push(TouchpadStreamEvent::DroppedFrames(1));
            self.active_contacts
                .get_or_insert_with(|| usize::from(finger_count));
        }
        events.extend(self.end_event());
        events
    }

    fn publish_frame(
        &mut self,
        frame: TouchFrame,
        now: Instant,
        events: &mut Vec<TouchpadStreamEvent>,
    ) {
        if self.last_timestamp_us.is_some_and(|previous| {
            frame.timestamp_us.saturating_sub(previous) >= self.stroke_end_timeout_us()
        }) {
            events.extend(self.end_event());
        }

        self.observe_cadence(frame.timestamp_us);
        self.active_contacts = Some(frame.contacts().len());
        self.last_frame_at = Some(now);
        events.push(TouchpadStreamEvent::Frame(frame));
    }

    fn observe_cadence(&mut self, timestamp_us: u64) {
        if let Some(previous) = self.last_timestamp_us {
            let sample = timestamp_us.saturating_sub(previous);
            if sample != 0 && sample <= MAX_STROKE_END_TIMEOUT_US {
                self.cadence_us = Some(self.cadence_us.map_or(sample, |current| {
                    current.saturating_mul(3).saturating_add(sample).div_ceil(4)
                }));
            }
        }
        self.last_timestamp_us = Some(timestamp_us);
    }

    fn stroke_end_timeout(&self) -> Duration {
        Duration::from_micros(self.stroke_end_timeout_us())
    }

    fn stroke_end_timeout_us(&self) -> u64 {
        self.cadence_us
            .map_or(FALLBACK_STROKE_END_TIMEOUT_US, |cadence| {
                cadence.saturating_mul(MISSED_FRAME_PERIODS)
            })
            .clamp(MIN_STROKE_END_TIMEOUT_US, MAX_STROKE_END_TIMEOUT_US)
    }

    fn end_event(&mut self) -> Option<TouchpadStreamEvent> {
        self.last_frame_at = None;
        self.last_timestamp_us = None;
        self.active_contacts
            .take()
            .map(|_| TouchpadStreamEvent::End)
    }
}

enum FrameOutcome {
    Frame(TouchFrame),
    Cancel { timestamp_us: u64, finger_count: u8 },
}

struct FrameAssembler {
    geometry: Geometry,
    pending: Option<PendingFrame>,
    rejected_timestamp: Option<u16>,
    timestamp: TimestampUnwrapper,
    dropped_frames: u64,
}

impl FrameAssembler {
    fn new(info: TouchpadInfo) -> Result<Self, TouchpadStreamError> {
        Ok(Self {
            geometry: Geometry::new(info)?,
            pending: None,
            rejected_timestamp: None,
            timestamp: TimestampUnwrapper::new(info.timestamp_units),
            dropped_frames: 0,
        })
    }

    fn push(&mut self, report: RawChunk) -> Option<FrameOutcome> {
        if report.spurious {
            self.reject(report.timestamp);
            return None;
        }
        if self.rejected_timestamp == Some(report.timestamp) {
            return None;
        }
        if self.rejected_timestamp.is_some() {
            self.rejected_timestamp = None;
        }
        if report.finger_count > 4 {
            let timestamp_us = self.timestamp.unwrap(report.timestamp);
            if self.pending.take().is_some() {
                self.dropped_frames = self.dropped_frames.saturating_add(1);
            }
            self.rejected_timestamp = Some(report.timestamp);
            return Some(FrameOutcome::Cancel {
                timestamp_us,
                finger_count: report.finger_count,
            });
        }
        if report.finger_count > self.geometry.max_finger_count {
            self.reject(report.timestamp);
            return None;
        }
        let starts_new_frame = self
            .pending
            .as_ref()
            .is_none_or(|pending| pending.timestamp != report.timestamp);
        if starts_new_frame {
            if self.pending.is_some() {
                self.dropped_frames = self.dropped_frames.saturating_add(1);
            }
            self.pending = Some(PendingFrame {
                timestamp: report.timestamp,
                timestamp_us: self.timestamp.unwrap(report.timestamp),
                finger_count: report.finger_count,
                button: report.button,
                contacts: Vec::with_capacity(usize::from(report.finger_count)),
            });
        }

        let pending = self.pending.as_mut()?;
        if pending.finger_count != report.finger_count || pending.button != report.button {
            self.reject(report.timestamp);
            return None;
        }
        for point in report.points {
            match self.geometry.normalize(point) {
                Ok(Some(contact)) if !pending.contacts.iter().any(|item| item.id == contact.id) => {
                    pending.contacts.push(contact);
                }
                Ok(None) => {}
                Ok(Some(_)) | Err(()) => {
                    self.reject(report.timestamp);
                    return None;
                }
            }
        }
        if !report.end_of_frame {
            return None;
        }

        let pending = self.pending.take()?;
        if pending.contacts.len() != usize::from(pending.finger_count) {
            self.rejected_timestamp = Some(report.timestamp);
            self.dropped_frames = self.dropped_frames.saturating_add(1);
            return None;
        }
        TouchFrame::new(pending.timestamp_us, pending.button, pending.contacts)
            .ok()
            .map(FrameOutcome::Frame)
    }

    fn reject(&mut self, timestamp: u16) {
        if self.rejected_timestamp != Some(timestamp) {
            self.dropped_frames = self.dropped_frames.saturating_add(1);
        }
        self.pending = None;
        self.rejected_timestamp = Some(timestamp);
    }

    fn take_dropped_frames(&mut self) -> u64 {
        std::mem::take(&mut self.dropped_frames)
    }

    fn drop_pending(&mut self) -> Option<u8> {
        let pending = self.pending.take()?;
        self.rejected_timestamp = Some(pending.timestamp);
        Some(pending.finger_count)
    }
}

struct PendingFrame {
    timestamp: u16,
    timestamp_us: u64,
    finger_count: u8,
    button: bool,
    contacts: Vec<TouchContact>,
}

#[derive(Clone, Copy)]
struct Geometry {
    x_size: u16,
    y_size: u16,
    dpi: u16,
    max_finger_count: u8,
    flip_x: bool,
    flip_y: bool,
}

impl Geometry {
    fn new(info: TouchpadInfo) -> Result<Self, TouchpadStreamError> {
        if info.dpi == 0 {
            return Err(TouchpadStreamError::ZeroDpi);
        }
        if info.timestamp_units == 0 {
            return Err(TouchpadStreamError::ZeroTimestampUnits);
        }
        if info.x_size == 0 || info.y_size == 0 {
            return Err(TouchpadStreamError::EmptyCoordinateRange);
        }
        // This Casa Touch — the device this feature targets — reports
        // mapping version 0 while following the expected DualXY layout, so
        // accept it alongside version 1 instead of failing capture arming.
        if !matches!(info.raw_report_mapping_version, 0 | 1) {
            return Err(TouchpadStreamError::UnsupportedMapping(
                info.raw_report_mapping_version,
            ));
        }
        let (flip_x, flip_y) = match info.origin {
            Origin::UpperLeft => (false, false),
            Origin::UpperRight => (true, false),
            Origin::LowerLeft => (false, true),
            Origin::LowerRight => (true, true),
            _ => return Err(TouchpadStreamError::UnsupportedMapping(1)),
        };
        Ok(Self {
            x_size: info.x_size,
            y_size: info.y_size,
            dpi: info.dpi,
            max_finger_count: info.max_finger_count,
            flip_x,
            flip_y,
        })
    }

    fn normalize(self, point: RawPoint) -> Result<Option<TouchContact>, ()> {
        if point.contact_status == 0 {
            return Ok(None);
        }
        if point.contact_status != 1
            || point.contact_type != 0
            || point.x > self.x_size
            || point.y > self.y_size
        {
            return Err(());
        }
        let x = if self.flip_x {
            self.x_size - point.x
        } else {
            point.x
        };
        let y = if self.flip_y {
            self.y_size - point.y
        } else {
            point.y
        };
        Ok(Some(TouchContact {
            id: point.finger_id,
            x_um: coordinate_um(x, self.dpi),
            y_um: coordinate_um(y, self.dpi),
        }))
    }
}

fn coordinate_um(value: u16, dpi: u16) -> u32 {
    let dpi = u64::from(dpi);
    u32::try_from((u64::from(value) * 25_400 + dpi / 2) / dpi).unwrap_or(u32::MAX)
}

struct TimestampUnwrapper {
    units_100us: u8,
    raw: Option<u16>,
    ticks: u64,
}

impl TimestampUnwrapper {
    fn new(units_100us: u8) -> Self {
        Self {
            units_100us,
            raw: None,
            ticks: 0,
        }
    }

    fn unwrap(&mut self, raw: u16) -> u64 {
        if let Some(previous) = self.raw {
            self.ticks = self
                .ticks
                .saturating_add(u64::from(raw.wrapping_sub(previous)));
        } else {
            self.ticks = u64::from(raw);
        }
        self.raw = Some(raw);
        self.ticks
            .saturating_mul(u64::from(self.units_100us))
            .saturating_mul(100)
    }
}

#[derive(Clone, Copy)]
struct RawPoint {
    contact_type: u8,
    contact_status: u8,
    x: u16,
    y: u16,
    finger_id: u8,
}

impl From<TouchPoint> for RawPoint {
    fn from(point: TouchPoint) -> Self {
        Self {
            contact_type: point.contact_type,
            contact_status: point.contact_status,
            x: point.x,
            y: point.y,
            finger_id: point.finger_id,
        }
    }
}

#[derive(Clone, Copy)]
struct RawChunk {
    timestamp: u16,
    points: [RawPoint; 2],
    button: bool,
    spurious: bool,
    end_of_frame: bool,
    finger_count: u8,
}

impl From<DualXyData> for RawChunk {
    fn from(report: DualXyData) -> Self {
        Self {
            timestamp: report.timestamp,
            points: [report.touch1.into(), report.touch2.into()],
            button: report.button,
            spurious: report.spurious,
            end_of_frame: report.end_of_frame,
            finger_count: report.finger_count,
        }
    }
}

/// Durable ownership record for one device's volatile `0x6100` raw mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawModeJournal {
    /// Mode observed before OpenLogi wrote anything.
    pub original: u8,
    /// Mode OpenLogi requested.
    pub requested: u8,
    /// Mode read back after the write, when that step completed.
    pub readback: Option<u8>,
    /// Whether the write and readback completed successfully.
    pub armed: bool,
}

/// A durable raw-mode journal store failed.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct TouchpadJournalError(String);

impl TouchpadJournalError {
    /// Wrap a host-store failure without exposing its platform-specific type.
    #[must_use]
    pub fn new(reason: impl std::fmt::Display) -> Self {
        Self(reason.to_string())
    }
}

/// Host persistence port for compare-and-restore raw-mode ownership.
pub trait TouchpadJournalStore: Send + Sync {
    /// Load the journal for a stable device identity.
    fn load(&self, device_id: &str) -> Result<Option<RawModeJournal>, TouchpadJournalError>;
    /// Atomically save the journal for a stable device identity.
    fn save(&self, device_id: &str, journal: RawModeJournal) -> Result<(), TouchpadJournalError>;
    /// Remove the journal once ownership has been resolved.
    fn clear(&self, device_id: &str) -> Result<(), TouchpadJournalError>;
}

/// Raw-mode state owned by one live session.
pub struct ArmedRawMode {
    /// `None` when the exact decodable mode was already active without an
    /// OpenLogi journal. In that case another manager owns it: listen, but
    /// never restore or otherwise write it.
    journal: Option<RawModeJournal>,
    expected: u8,
}

impl ArmedRawMode {
    /// Resolve a journal left by an interrupted session without arming a new
    /// one. Used when capture is now disabled so startup still restores raw
    /// mode before leaving the device native.
    pub async fn recover(
        feature: &TouchpadRawXyFeature,
        store: &dyn TouchpadJournalStore,
        device_id: &str,
    ) -> Result<(), TouchpadCaptureError> {
        recover_raw_mode(feature, store, device_id).await
    }

    /// Arm raw reporting after recovering any prior interrupted session.
    pub async fn arm(
        feature: &TouchpadRawXyFeature,
        store: &dyn TouchpadJournalStore,
        device_id: &str,
    ) -> Result<Self, TouchpadCaptureError> {
        recover_raw_mode(feature, store, device_id).await?;

        let original = feature
            .get_raw_report_state()
            .await
            .map_err(protocol_error)?;
        let requested = requested_raw_report_flags();
        if original.contains(RawReportFlags::RAW) {
            if original != requested {
                return Err(TouchpadCaptureError::ExternalRawMode {
                    actual: original.bits(),
                });
            }
            return Ok(Self {
                journal: None,
                expected: original.bits(),
            });
        }
        let mut journal = RawModeJournal {
            original: original.bits(),
            requested: requested.bits(),
            readback: None,
            armed: false,
        };
        store.save(device_id, journal)?;
        log_gestures_handling(feature).await;
        feature
            .set_raw_report_state(requested)
            .await
            .map_err(protocol_error)?;
        let readback = feature
            .get_raw_report_state()
            .await
            .map_err(protocol_error)?;
        journal.readback = Some(readback.bits());
        journal.armed = readback == requested;
        if let Err(error) = store.save(device_id, journal) {
            let _ = compare_and_restore(feature, store, device_id, journal).await;
            return Err(error.into());
        }
        if readback != requested {
            compare_and_restore(feature, store, device_id, journal).await?;
            return Err(TouchpadCaptureError::Readback {
                requested: requested.bits(),
                actual: readback.bits(),
            });
        }
        Ok(Self {
            journal: Some(journal),
            expected: readback.bits(),
        })
    }

    /// Raw-report flags this live session may continue listening under.
    #[must_use]
    pub const fn expected(&self) -> u8 {
        self.expected
    }

    /// Restore the pre-session mode only if the device still carries the mode
    /// this session wrote.
    pub async fn disarm(
        self,
        feature: &TouchpadRawXyFeature,
        store: &dyn TouchpadJournalStore,
        device_id: &str,
    ) -> Result<(), TouchpadCaptureError> {
        match self.journal {
            Some(journal) => compare_and_restore(feature, store, device_id, journal).await,
            None => Ok(()),
        }
    }
}

/// Failure preparing or restoring `0x6100` raw capture.
#[derive(Debug, Error)]
pub enum TouchpadCaptureError {
    /// HID++ feature call failed.
    #[error("HID++ touchpad protocol error: {0}")]
    Protocol(String),
    /// Durable journal access failed.
    #[error("touchpad raw-mode journal error: {0}")]
    Journal(#[from] TouchpadJournalError),
    /// The mode read back did not match what OpenLogi requested.
    #[error("touchpad raw mode readback mismatch: requested {requested:#04x}, got {actual:#04x}")]
    Readback {
        /// Requested flag bitmap.
        requested: u8,
        /// Read-back flag bitmap.
        actual: u8,
    },
    /// Another manager already owns an incompatible raw-report layout.
    #[error("touchpad raw mode is externally owned with incompatible flags {actual:#04x}")]
    ExternalRawMode {
        /// Externally owned flag bitmap.
        actual: u8,
    },
}

async fn recover_raw_mode(
    feature: &TouchpadRawXyFeature,
    store: &dyn TouchpadJournalStore,
    device_id: &str,
) -> Result<(), TouchpadCaptureError> {
    let Some(journal) = store.load(device_id)? else {
        return Ok(());
    };
    compare_and_restore(feature, store, device_id, journal).await
}

/// Read and log the firmware's per-gesture handling bitmap — the state the
/// parity regime deliberately rides on unmodified. Read-only: OpenLogi never
/// rewrites it, so a device that does not expose the read only loses the log
/// line.
async fn log_gestures_handling(feature: &TouchpadRawXyFeature) {
    match feature.get_gestures_handling_output().await {
        Ok(bytes) => {
            tracing::info!(
                handling = ?bytes,
                "touchpad gestures-handling bitmap (byte2 b4-5 = 3F drag, byte3 b6-7 = 1F tap-and-hold drag)"
            );
        }
        Err(error) => {
            tracing::debug!(error = ?error, "gestures-handling bitmap unreadable");
        }
    }
}

/// Restore the pre-session mode only if the device still carries the mode
/// this session wrote.
async fn compare_and_restore(
    feature: &TouchpadRawXyFeature,
    store: &dyn TouchpadJournalStore,
    device_id: &str,
    journal: RawModeJournal,
) -> Result<(), TouchpadCaptureError> {
    let current = feature
        .get_raw_report_state()
        .await
        .map_err(protocol_error)?;
    let owned = if journal.armed {
        journal.readback.unwrap_or(journal.requested)
    } else {
        journal.requested
    };
    if current.bits() == owned && current.bits() != journal.original {
        feature
            .set_raw_report_state(RawReportFlags::from_bits_retain(journal.original))
            .await
            .map_err(protocol_error)?;
        let restored = feature
            .get_raw_report_state()
            .await
            .map_err(protocol_error)?;
        if restored.bits() != journal.original {
            return Err(TouchpadCaptureError::Readback {
                requested: journal.original,
                actual: restored.bits(),
            });
        }
    }
    // A different current value belongs to firmware or another manager. Do
    // not fight it; ownership is resolved by dropping our stale journal.
    store.clear(device_id)?;
    Ok(())
}

fn protocol_error(error: impl std::fmt::Debug) -> TouchpadCaptureError {
    TouchpadCaptureError::Protocol(format!("{error:?}"))
}
