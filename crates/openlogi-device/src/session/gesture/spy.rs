//! `0x8110` spy capture: Host-mode mapping suppress and mask-diff.
//!
//! Bit tables are keyed by the same HID++ model id as
//! `openlogi_core::binding::SPY_MODELS`. The G502 X Plus row (`04099`) is
//! locked from a live `diag mouse-buttons --watch`. Do not copy it onto a
//! cousin. Function semantics are reverse-engineered from public cvuchener
//! `IMouseButtonSpy` / `IOnboardProfiles` descriptions; this file does not
//! copy GPL code.
//!
//! Adding a model: dump bits, add a [`SPY_BIT_TABLES`] row with that key, and
//! add the matching [`openlogi_core::binding::SpyModel`]. G4/G5 belong in the
//! bit table when the mouse has no `0x1b04` — remaps then suppress those
//! mapping slots so the OS stops seeing hardware Back/Forward.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature,
        mouse_button_filter::MouseButtonFilterFeature,
        onboard_profiles::{OnboardProfilesFeature, OnboardProfilesMode},
    },
    protocol::v20,
};
use openlogi_core::binding::{ButtonId, G502_X_PLUS_CONFIG_KEY};
use tracing::{debug, warn};

use super::{CaptureSpec, GestureError};
use crate::session::capture_restore::restore_result;

/// Locked `0x8110` bit → [`ButtonId`] map for the G502 X Plus.
///
/// G4/G5 (bits 3 and 5) are suppressed only when those buttons are in the
/// armed set — unbound they stay firmware-native.
const G502_X_PLUS_SPY_BITS: [(u8, ButtonId); 6] = [
    (3, ButtonId::Back),
    (4, ButtonId::DpiShift),
    (5, ButtonId::Forward),
    (10, ButtonId::DpiDown),
    (9, ButtonId::DpiUp),
    (8, ButtonId::ProfileCycle),
];

/// Same HID++ keys as `openlogi_core::binding::SPY_MODELS`. Bits are
/// per-model; two cousins can share [`ButtonId`]s with different masks.
const SPY_BIT_TABLES: &[(&str, &[(u8, ButtonId)])] =
    &[(G502_X_PLUS_CONFIG_KEY, &G502_X_PLUS_SPY_BITS)];

fn spy_bits_for_model(key: &str) -> &'static [(u8, ButtonId)] {
    SPY_BIT_TABLES
        .iter()
        .find(|(model, _)| *model == key)
        .map_or(&[], |(_, bits)| *bits)
}

/// Decode an unsolicited `0x8110` spy event on this session's channel.
///
/// Returns `None` for request responses and messages from a different device
/// or feature. Bit 0 is the first spy-button slot; the mask is big-endian.
#[must_use]
pub fn decode_event(msg: &v20::Message, device_index: u8, feature_index: u8) -> Option<u16> {
    let header = msg.header();
    if header.device_index != device_index
        || header.feature_index != feature_index
        || header.software_id.to_lo() != 0
        || header.function_id.to_lo() != 0
    {
        return None;
    }
    let p = msg.extend_payload();
    Some(u16::from_be_bytes([p[0], p[1]]))
}

/// Rising and falling edges between two spy masks, in table order.
#[must_use]
pub fn spy_edges(
    previous: u16,
    next: u16,
    buttons: &[ButtonId],
    bits: &[(u8, ButtonId)],
) -> Vec<(ButtonId, bool)> {
    let changed = previous ^ next;
    bits.iter()
        .copied()
        .filter(|(bit, button)| changed & (1 << bit) != 0 && buttons.contains(button))
        .map(|(bit, button)| (button, next & (1 << bit) != 0))
        .collect()
}

/// Zero the HID mapping slots that belong to `buttons`.
#[must_use]
pub fn suppress_spy_slots(
    mapping: &[u8],
    buttons: &[ButtonId],
    bits: &[(u8, ButtonId)],
) -> Vec<u8> {
    let mut out = mapping.to_vec();
    for (bit, button) in bits {
        if !buttons.contains(button) {
            continue;
        }
        if let Some(slot) = out.get_mut(usize::from(*bit)) {
            *slot = 0;
        }
    }
    out
}

/// Firmware ownership for a Host-mode spy session.
pub(super) struct ArmedSpy {
    filter: Arc<MouseButtonFilterFeature>,
    filter_index: u8,
    profiles: Option<Arc<OnboardProfilesFeature>>,
    profiles_index: Option<u8>,
    original_mapping: Vec<u8>,
    original_mode: Option<OnboardProfilesMode>,
    buttons: Vec<ButtonId>,
    bits: &'static [(u8, ButtonId)],
    spy_started: bool,
    released: bool,
    /// True after a Host-mode or mapping write. Restore and [`Drop`] only
    /// compensate when firmware may have changed.
    dirty: bool,
}

/// Opaque restore token handed to [`super::PendingCaptureRestore`].
pub(crate) struct SpyRestore {
    filter_index: u8,
    profiles_index: Option<u8>,
    mapping: Vec<u8>,
    mode: Option<OnboardProfilesMode>,
    stop_spy: bool,
}

impl ArmedSpy {
    /// Snapshot mapping and mode. Does **not** write firmware — store the
    /// result on [`super::ArmedControls`] before [`Self::apply`] so a failed
    /// Host-mode or mapping write still has a rollback token.
    pub(super) async fn prepare(
        device: &Device,
        chan: &Arc<HidppChannel>,
        device_index: u8,
        spec: &CaptureSpec,
    ) -> Result<Option<Self>, GestureError> {
        if spec.spy_buttons.is_empty() {
            return Ok(None);
        }

        let filter_index = match device
            .root()
            .get_feature(MouseButtonFilterFeature::ID)
            .await
        {
            Ok(Some(info)) => info.index,
            Ok(None) => {
                warn!("spy buttons requested but 0x8110 is absent — Host-mode remap skipped");
                return Ok(None);
            }
            Err(error) => return Err(GestureError::Hidpp(format!("{error:?}"))),
        };
        let filter = Arc::new(MouseButtonFilterFeature::new(
            Arc::clone(chan),
            device_index,
            filter_index,
        ));

        let profiles_index = match device.root().get_feature(OnboardProfilesFeature::ID).await {
            Ok(Some(info)) => info.index,
            Ok(None) => {
                warn!("0x8100 is absent — 0x8110 mapping cannot apply; spy remap skipped");
                return Ok(None);
            }
            Err(error) => return Err(GestureError::Hidpp(format!("{error:?}"))),
        };
        let profiles = Some(Arc::new(OnboardProfilesFeature::new(
            Arc::clone(chan),
            device_index,
            profiles_index,
        )));
        let profiles_index = Some(profiles_index);

        let original_mapping = filter
            .get_mouse_button_mapping()
            .await
            .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?;
        let original_mode = match profiles.as_ref() {
            Some(profiles) => Some(
                profiles
                    .get_mode()
                    .await
                    .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?,
            ),
            None => None,
        };

        let bits = spec
            .spy_model_key
            .as_deref()
            .map_or(&[][..], spy_bits_for_model);
        Ok(Some(Self {
            filter,
            filter_index,
            profiles,
            profiles_index,
            original_mapping,
            original_mode,
            buttons: spec.spy_buttons.clone(),
            bits,
            spy_started: false,
            released: false,
            dirty: false,
        }))
    }

    /// Enter Host mode and zero armed mapping slots.
    ///
    /// Does **not** start the spy stream: the channel listener must be
    /// registered first so the first mask is not dropped.
    pub(super) async fn apply(&mut self) -> Result<(), GestureError> {
        if self.original_mode == Some(OnboardProfilesMode::Onboard)
            && let Some(profiles) = self.profiles.as_ref()
        {
            profiles
                .set_mode(OnboardProfilesMode::Host)
                .await
                .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?;
            self.dirty = true;
        }

        let suppressed = suppress_spy_slots(&self.original_mapping, &self.buttons, self.bits);
        if suppressed != self.original_mapping {
            self.filter
                .set_mouse_button_mapping(&suppressed)
                .await
                .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?;
            self.dirty = true;
        }
        Ok(())
    }

    pub(super) fn feature_index(&self) -> u8 {
        self.filter_index
    }

    pub(super) fn buttons(&self) -> &[ButtonId] {
        &self.buttons
    }

    pub(super) fn bits(&self) -> &'static [(u8, ButtonId)] {
        self.bits
    }

    /// Start the spy after the session listener owns inbound reports.
    pub(super) async fn start(&mut self) -> Result<(), GestureError> {
        self.filter
            .start_mouse_button_spy()
            .await
            .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?;
        self.spy_started = true;
        self.dirty = true;
        Ok(())
    }

    /// Re-enter Host mode and re-zero mapping after a wireless reconnect.
    ///
    /// A failed spy start after those writes is an error: Host mode plus a
    /// suppressed mapping with no `0x8110` stream is the same dead-button
    /// state as a failed first start, so the session must restart and roll back.
    pub(super) async fn rearm(&self) -> Result<(), GestureError> {
        if let Some(profiles) = self.profiles.as_ref()
            && let Err(error) = profiles.set_mode(OnboardProfilesMode::Host).await
        {
            warn!(?error, "Host mode re-arm after wake failed");
        }
        let suppressed = suppress_spy_slots(&self.original_mapping, &self.buttons, self.bits);
        if let Err(error) = self.filter.set_mouse_button_mapping(&suppressed).await {
            warn!(?error, "spy mapping re-arm after wake failed");
        }
        self.filter
            .start_mouse_button_spy()
            .await
            .map_err(|error| GestureError::Hidpp(format!("{error:?}")))
    }

    /// Restore mapping then onboard mode. Marks the guard released so
    /// [`Drop`] does not fire a second restore.
    pub(super) async fn restore(&mut self) -> bool {
        let restored = restore_spy_state(
            &self.filter,
            self.profiles.as_deref(),
            &self.original_mapping,
            self.original_mode,
            self.spy_started,
        )
        .await;
        if restored {
            self.released = true;
            self.spy_started = false;
        }
        restored
    }

    pub(super) fn into_restore(mut self) -> Option<SpyRestore> {
        if self.released || !self.dirty {
            return None;
        }
        let restore = SpyRestore {
            filter_index: self.filter_index,
            profiles_index: self.profiles_index,
            mapping: self.original_mapping.clone(),
            mode: self.original_mode,
            stop_spy: self.spy_started,
        };
        self.released = true;
        Some(restore)
    }
}

impl Drop for ArmedSpy {
    fn drop(&mut self) {
        if self.released || !self.dirty {
            return;
        }
        let filter = Arc::clone(&self.filter);
        let profiles = self.profiles.clone();
        let mapping = self.original_mapping.clone();
        let mode = self.original_mode;
        let stop_spy = self.spy_started;
        self.released = true;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            drop(handle.spawn(async move {
                let _ =
                    restore_spy_state(&filter, profiles.as_deref(), &mapping, mode, stop_spy).await;
            }));
        }
    }
}

impl SpyRestore {
    pub(crate) async fn restore_on(&self, channel: &Arc<HidppChannel>, device_index: u8) -> bool {
        let filter =
            MouseButtonFilterFeature::new(Arc::clone(channel), device_index, self.filter_index);
        let profiles = self
            .profiles_index
            .map(|index| OnboardProfilesFeature::new(Arc::clone(channel), device_index, index));
        restore_spy_state(
            &filter,
            profiles.as_ref(),
            &self.mapping,
            self.mode,
            self.stop_spy,
        )
        .await
    }
}

async fn restore_spy_state(
    filter: &MouseButtonFilterFeature,
    profiles: Option<&OnboardProfilesFeature>,
    mapping: &[u8],
    mode: Option<OnboardProfilesMode>,
    stop_spy: bool,
) -> bool {
    let mut restored = true;
    if stop_spy {
        restored &= restore_result(filter.stop_mouse_button_spy().await, "mouse button spy");
    }
    restored &= restore_result(
        filter.set_mouse_button_mapping(mapping).await,
        "mouse button mapping",
    );
    if let (Some(profiles), Some(mode)) = (profiles, mode) {
        restored &= restore_result(profiles.set_mode(mode).await, "onboard profiles mode");
    } else {
        debug!("no 0x8100 mode to restore");
    }
    restored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_report_g8_down_then_up() {
        let buttons = ButtonId::SPY_BUTTONS;
        let down = spy_edges(0, 0x0200, &buttons, &G502_X_PLUS_SPY_BITS);
        assert_eq!(down, [(ButtonId::DpiUp, true)]);
        let up = spy_edges(0x0200, 0, &buttons, &G502_X_PLUS_SPY_BITS);
        assert_eq!(up, [(ButtonId::DpiUp, false)]);
    }

    #[test]
    fn edges_ignore_g4_and_g5_unless_those_buttons_are_armed() {
        let extras_only = spy_edges(
            0,
            0x0008 | 0x0020,
            &ButtonId::SPY_BUTTONS,
            &G502_X_PLUS_SPY_BITS,
        );
        assert!(extras_only.is_empty());
        let with_back = spy_edges(0, 0x0008, &[ButtonId::Back], &G502_X_PLUS_SPY_BITS);
        assert_eq!(with_back, [(ButtonId::Back, true)]);
    }

    #[test]
    fn suppress_zeros_only_armed_slots() {
        let mapping: Vec<u8> = (1..=11).collect();
        let extras = suppress_spy_slots(&mapping, &ButtonId::SPY_BUTTONS, &G502_X_PLUS_SPY_BITS);
        assert_eq!(&extras[..4], &[1, 2, 3, 4]);
        assert_eq!(extras[4], 0);
        assert_eq!(extras[5], 6);
        assert_eq!(extras[6], 7);
        assert_eq!(extras[7], 8);
        assert_eq!(extras[8], 0);
        assert_eq!(extras[9], 0);
        assert_eq!(extras[10], 0);

        let with_back = suppress_spy_slots(&mapping, &[ButtonId::Back], &G502_X_PLUS_SPY_BITS);
        assert_eq!(with_back[3], 0);
        assert_eq!(with_back[5], 6);
    }
}
