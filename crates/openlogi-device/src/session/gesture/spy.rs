//! G502 X Plus `0x8110` spy capture: Host-mode mapping suppress and mask-diff.
//!
//! Bit assignments are locked from a live `diag mouse-buttons --watch` on
//! this G502 X Plus (`config_key` `04099`). Other G-series maps are not
//! invented here. Function semantics are reverse-engineered from public
//! cvuchener `IMouseButtonSpy` / `IOnboardProfiles` descriptions; this file
//! does not copy GPL code.

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
use openlogi_core::binding::ButtonId;
use tracing::{debug, warn};

use super::{CaptureSpec, GestureError};
use crate::session::capture_restore::restore_result;

/// Locked `0x8110` bit → [`ButtonId`] map for the G502 X Plus.
///
/// G4/G5 (bits 3 and 5) are OS-hook only and are never listed here.
const G502_X_PLUS_SPY_BITS: [(u8, ButtonId); 4] = [
    (4, ButtonId::DpiShift),
    (10, ButtonId::DpiDown),
    (9, ButtonId::DpiUp),
    (8, ButtonId::ProfileCycle),
];

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

/// Rising and falling edges between two spy masks, in declaration order.
#[must_use]
pub fn spy_edges(previous: u16, next: u16, buttons: &[ButtonId]) -> Vec<(ButtonId, bool)> {
    let changed = previous ^ next;
    G502_X_PLUS_SPY_BITS
        .into_iter()
        .filter(|(bit, button)| changed & (1 << bit) != 0 && buttons.contains(button))
        .map(|(bit, button)| (button, next & (1 << bit) != 0))
        .collect()
}

/// Zero the HID mapping slots that belong to `buttons`, leaving G1–G5 alone.
#[must_use]
pub fn suppress_spy_slots(mapping: &[u8], buttons: &[ButtonId]) -> Vec<u8> {
    let mut out = mapping.to_vec();
    for (bit, button) in G502_X_PLUS_SPY_BITS {
        if !buttons.contains(&button) {
            continue;
        }
        if let Some(slot) = out.get_mut(usize::from(bit)) {
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
    spy_started: bool,
    released: bool,
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
    /// Enter Host mode, snapshot the mapping, and zero G6–G9 slots.
    ///
    /// Does **not** start the spy stream: the channel listener must be
    /// registered first so the first mask is not dropped.
    pub(super) async fn arm(
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

        let armed = Self {
            filter,
            filter_index,
            profiles,
            profiles_index,
            original_mapping: original_mapping.clone(),
            original_mode,
            buttons: spec.spy_buttons.clone(),
            spy_started: false,
            released: false,
        };

        if original_mode == Some(OnboardProfilesMode::Onboard)
            && let Some(profiles) = armed.profiles.as_ref()
        {
            profiles
                .set_mode(OnboardProfilesMode::Host)
                .await
                .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?;
        }

        let suppressed = suppress_spy_slots(&original_mapping, &spec.spy_buttons);
        if suppressed != original_mapping {
            armed
                .filter
                .set_mouse_button_mapping(&suppressed)
                .await
                .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?;
        }
        Ok(Some(armed))
    }

    pub(super) fn feature_index(&self) -> u8 {
        self.filter_index
    }

    pub(super) fn buttons(&self) -> &[ButtonId] {
        &self.buttons
    }

    /// Start the spy after the session listener owns inbound reports.
    pub(super) async fn start(&mut self) -> Result<(), GestureError> {
        self.filter
            .start_mouse_button_spy()
            .await
            .map_err(|error| GestureError::Hidpp(format!("{error:?}")))?;
        self.spy_started = true;
        Ok(())
    }

    /// Re-enter Host mode and re-zero mapping after a wireless reconnect.
    pub(super) async fn rearm(&self) {
        if let Some(profiles) = self.profiles.as_ref()
            && let Err(error) = profiles.set_mode(OnboardProfilesMode::Host).await
        {
            warn!(?error, "Host mode re-arm after wake failed");
        }
        let suppressed = suppress_spy_slots(&self.original_mapping, &self.buttons);
        if let Err(error) = self.filter.set_mouse_button_mapping(&suppressed).await {
            warn!(?error, "spy mapping re-arm after wake failed");
        }
        if let Err(error) = self.filter.start_mouse_button_spy().await {
            warn!(?error, "spy start after wake failed");
        }
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
        if self.released {
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
        if self.released {
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
        let down = spy_edges(0, 0x0200, &buttons);
        assert_eq!(down, [(ButtonId::DpiUp, true)]);
        let up = spy_edges(0x0200, 0, &buttons);
        assert_eq!(up, [(ButtonId::DpiUp, false)]);
    }

    #[test]
    fn edges_ignore_g4_and_g5_bits() {
        // bit 3 = G4 Back, bit 5 = G5 Forward — OS hook only.
        let edges = spy_edges(0, 0x0008 | 0x0020, &ButtonId::SPY_BUTTONS);
        assert!(edges.is_empty());
    }

    #[test]
    fn suppress_zeros_only_g6_through_g9() {
        let mapping: Vec<u8> = (1..=11).collect();
        let suppressed = suppress_spy_slots(&mapping, &ButtonId::SPY_BUTTONS);
        assert_eq!(&suppressed[..4], &[1, 2, 3, 4]);
        assert_eq!(suppressed[4], 0);
        assert_eq!(suppressed[5], 6);
        assert_eq!(suppressed[6], 7);
        assert_eq!(suppressed[7], 8);
        assert_eq!(suppressed[8], 0);
        assert_eq!(suppressed[9], 0);
        assert_eq!(suppressed[10], 0);
    }
}
