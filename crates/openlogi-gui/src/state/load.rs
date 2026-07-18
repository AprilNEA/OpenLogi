//! Lazy per-device load state for background HID++ reads, shared by DPI
//! capability discovery and SmartShift reads.

use std::collections::BTreeMap;

use openlogi_hid::{DpiInfo, SmartShiftStatus, WriteError};
use tracing::debug;

/// How many times to retry a device read (DPI capability discovery or a
/// SmartShift read) after a transient HID++ error (read timeout, busy device)
/// before giving up. A genuine "feature not supported" reply is permanent and
/// never retried.
const LOAD_MAX_ATTEMPTS: u8 = 3;

/// Lazy per-device load state for a background HID++ read: unqueried, in flight,
/// resolved, transiently failed (retryable on re-select), or permanently
/// unsupported. Shared by DPI capability discovery and SmartShift reads through
/// [`LazyDeviceData`]; the two differ only in payload type `T` and in which
/// errors count as permanent.
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
    /// the feature — re-selecting it (see
    /// [`AppState::set_current_device`](super::AppState::set_current_device))
    /// grants a fresh attempt.
    Failed(String),
    /// The device genuinely does not support the feature; never retried.
    Unsupported(String),
}

/// Per-device DPI capability load state. See [`Load`].
pub type DpiStatus = Load<DpiInfo>;

/// Per-device SmartShift (`0x2111`) config load state. See [`Load`]. Unlike DPI
/// presets, the resolved config is *not* persisted to `config.toml` — the device
/// stores wheel mode / threshold / torque in its own non-volatile memory, so the
/// GUI only ever reads and writes the device.
pub type SmartShiftLoad = Load<SmartShiftStatus>;

/// Per-device lazy-load cache for a background HID++ read, keyed by
/// [`DeviceRecord::config_key`](super::DeviceRecord::config_key). Holds each
/// device's [`Load`] state plus its transient-retry counter, and carries the
/// stale-route guard + retry-budget policy once, for both DPI and SmartShift.
pub(super) struct LazyDeviceData<T> {
    by_device: BTreeMap<String, Load<T>>,
    /// Consecutive transient read failures per device, capped by
    /// [`LOAD_MAX_ATTEMPTS`] before the device settles on [`Load::Failed`].
    attempts: BTreeMap<String, u8>,
}

// Manual `Default` (not derived): a derive would demand `T: Default`, but the
// empty maps need nothing of `T`.
impl<T> Default for LazyDeviceData<T> {
    fn default() -> Self {
        Self {
            by_device: BTreeMap::new(),
            attempts: BTreeMap::new(),
        }
    }
}

impl<T: Clone> LazyDeviceData<T> {
    /// The recorded state for `key`, or [`Load::Unknown`] if never queried.
    pub(super) fn status(&self, key: &str) -> Load<T> {
        self.by_device.get(key).cloned().unwrap_or(Load::Unknown)
    }

    /// The raw recorded entry for `key`, for callers that match on `Ready`
    /// without cloning the payload.
    pub(super) fn get(&self, key: &str) -> Option<&Load<T>> {
        self.by_device.get(key)
    }

    /// Whether `key` still needs a read (nothing recorded yet). Cheaper than
    /// cloning [`status`](Self::status) on the per-frame render path.
    pub(super) fn unqueried(&self, key: &str) -> bool {
        !self.by_device.contains_key(key)
    }

    /// Mark a read as in flight for `key`.
    pub(super) fn mark_loading(&mut self, key: &str) {
        self.by_device.insert(key.to_string(), Load::Loading);
    }

    /// Reset a stuck `Loading` for `key` back to unqueried — the read worker
    /// vanished (e.g. panicked) without delivering a result, so the next render
    /// re-issues instead of wedging the device on "Reading…".
    pub(super) fn clear_loading(&mut self, key: &str) {
        if matches!(self.by_device.get(key), Some(Load::Loading)) {
            self.by_device.remove(key);
        }
    }

    /// Drop `key`'s recorded state and retry budget so the next render re-reads.
    /// Backs the "click to retry" affordance and the re-select-grants-a-retry
    /// rule for a [`Load::Failed`] device.
    pub(super) fn retry(&mut self, key: &str) {
        self.by_device.remove(key);
        self.attempts.remove(key);
    }

    /// Forget `key` entirely — the device disappeared, or reconnected on a new
    /// route, so its cached state (keyed to the dead route) is stale.
    pub(super) fn remove(&mut self, key: &str) {
        self.by_device.remove(key);
        self.attempts.remove(key);
    }

    /// Forget every device the `present` predicate rejects (not in the live set).
    pub(super) fn retain_present(&mut self, present: impl Fn(&str) -> bool) {
        self.by_device.retain(|key, _| present(key.as_str()));
        self.attempts.retain(|key, _| present(key.as_str()));
    }

    /// Optimistically record a resolved value with no read involved — e.g. a
    /// just-written SmartShift config, shown until a confirming re-read replaces
    /// it. Leaves the retry budget untouched.
    pub(super) fn set_ready(&mut self, key: String, value: T) {
        self.by_device.insert(key, Load::Ready(value));
    }

    /// Store a read result under the stale-route guard and the transient-retry /
    /// permanent-unsupported policy. `matches_route` is whether a live device
    /// still holds `key` *on the route the read targeted*; `still_present` is
    /// whether `key` exists at all. Returns the resolved value when the result
    /// settled to [`Load::Ready`], so the caller can run a side effect (the DPI
    /// panel seeds the shared current value). `label` tags the debug logs.
    pub(super) fn store(
        &mut self,
        key: String,
        result: Result<T, WriteError>,
        is_permanent: impl Fn(&WriteError) -> bool,
        matches_route: bool,
        still_present: bool,
        label: &'static str,
    ) -> Option<T> {
        if !matches_route {
            debug!(key, label, "stale device read result ignored");
            // The device reconnected on a different route mid-read: drop the
            // orphaned `Loading` marker so the next render re-reads against the
            // live route instead of spinning on "Reading…" forever.
            if still_present {
                self.by_device.remove(&key);
            }
            return None;
        }

        let status = match result {
            Ok(value) => {
                self.attempts.remove(&key);
                Load::Ready(value)
            }
            // A genuine "feature not supported" reply never changes — record it
            // and stop probing.
            Err(error) if is_permanent(&error) => {
                self.attempts.remove(&key);
                Load::Unsupported(error.to_string())
            }
            // Transient failures get a few more tries: clear the status so the
            // next render re-reads, until the budget runs out, then settle on
            // `Failed` (retryable on re-select) rather than `Unsupported`.
            Err(error) => {
                let attempts = self.attempts.entry(key.clone()).or_insert(0);
                *attempts = attempts.saturating_add(1);
                if *attempts < LOAD_MAX_ATTEMPTS {
                    debug!(key, attempts = *attempts, error = %error, label, "transient device read error — will retry");
                    self.by_device.remove(&key);
                    return None;
                }
                self.attempts.remove(&key);
                Load::Failed(error.to_string())
            }
        };

        // Clone out the resolved value (cheap; once per completed read) before
        // the status moves into the map, so the caller can seed derived state
        // without re-borrowing `self`.
        let resolved = match &status {
            Load::Ready(value) => Some(value.clone()),
            _ => None,
        };
        self.by_device.insert(key, status);
        resolved
    }
}
