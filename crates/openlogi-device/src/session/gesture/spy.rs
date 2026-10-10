//! G-series host mode: captured buttons are muted in the `0x8110` mapping and
//! dispatched from spy reports.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature,
        mouse_button_spy::{MouseButtonSpyEvent, MouseButtonSpyFeature},
        onboard_profiles::{OnboardMode, OnboardProfilesFeature},
        report_rate::ReportRateFeature,
    },
    protocol::v20::{self, Hidpp20Error},
};
use openlogi_core::binding::ButtonId;
use openlogi_core::hid::ReportRate;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::CapturedInput;
use crate::session::capture_restore::CaptureError;

/// Onboard state a capture session sets up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnboardTarget {
    /// Host mode.
    Host(HostMode),
    /// An onboard profile (1-based).
    Profile(u8),
}

/// Host-mode capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostMode {
    /// Every slot, and whether it is captured.
    pub slots: Vec<(ButtonId, bool)>,
    /// Report rate to set.
    pub report_rate: Option<ReportRate>,
}

impl HostMode {
    fn mapping(&self) -> Vec<u8> {
        self.slots
            .iter()
            .map(|&(button, captured)| {
                if captured {
                    0
                } else {
                    native_hid_button(button)
                }
            })
            .collect()
    }

    fn captured(&self) -> impl Iterator<Item = (u8, ButtonId)> + '_ {
        self.slots
            .iter()
            .zip(0u8..)
            .filter(|((_, captured), _)| *captured)
            .map(|(&(button, _), slot)| (slot, button))
    }
}

// Checked on a G502 LIGHTSPEED: only these send anything natively.
fn native_hid_button(button: ButtonId) -> u8 {
    match button {
        ButtonId::LeftClick => 1,
        ButtonId::RightClick => 2,
        ButtonId::MiddleClick => 3,
        ButtonId::Back => 4,
        ButtonId::Forward => 5,
        _ => 0,
    }
}

pub(super) struct ArmedOnboard {
    chan: Arc<HidppChannel>,
    device_index: u8,
    onboard_index: u8,
    mode: ArmedMode,
}

enum ArmedMode {
    Host {
        spy_index: u8,
        rate_index: Option<u8>,
        host: HostMode,
        captured: Vec<(u8, ButtonId)>,
    },
    Profile(u8),
}

impl ArmedOnboard {
    pub(super) fn spy(&self) -> Option<(u8, Vec<(u8, ButtonId)>)> {
        match &self.mode {
            ArmedMode::Host {
                spy_index,
                captured,
                ..
            } => Some((*spy_index, captured.clone())),
            ArmedMode::Profile(_) => None,
        }
    }

    pub(super) fn restore(&self) -> Option<SpyRestore> {
        match &self.mode {
            ArmedMode::Host { spy_index, .. } => Some(SpyRestore {
                onboard_index: self.onboard_index,
                spy_index: *spy_index,
            }),
            ArmedMode::Profile(_) => None,
        }
    }

    pub(super) fn captured_count(&self) -> usize {
        self.spy().map_or(0, |(_, captured)| captured.len())
    }

    pub(super) async fn rearm(&self) -> bool {
        let onboard = self.onboard();
        let result = match &self.mode {
            ArmedMode::Host {
                spy_index,
                rate_index,
                host,
                ..
            } => {
                self.apply_host(&onboard, *spy_index, *rate_index, host)
                    .await
            }
            ArmedMode::Profile(index) => apply_profile(&onboard, *index).await,
        };
        if let Err(error) = &result {
            warn!(?error, "onboard re-arm after wake failed");
        }
        result.is_ok()
    }

    fn onboard(&self) -> OnboardProfilesFeature {
        OnboardProfilesFeature::new(
            Arc::clone(&self.chan),
            self.device_index,
            self.onboard_index,
        )
    }

    async fn apply_host(
        &self,
        onboard: &OnboardProfilesFeature,
        spy_index: u8,
        rate_index: Option<u8>,
        host: &HostMode,
    ) -> Result<(), Hidpp20Error> {
        onboard.set_mode(OnboardMode::Host).await?;
        let spy = MouseButtonSpyFeature::new(Arc::clone(&self.chan), self.device_index, spy_index);
        spy.set_mapping(&host.mapping()).await?;
        spy.start_spy().await?;
        if let (Some(index), Some(rate)) = (rate_index, host.report_rate) {
            let feature = ReportRateFeature::new(Arc::clone(&self.chan), self.device_index, index);
            feature.set_report_rate(rate.ms()).await?;
        }
        Ok(())
    }
}

async fn apply_profile(onboard: &OnboardProfilesFeature, index: u8) -> Result<(), Hidpp20Error> {
    if onboard.get_mode().await? != OnboardMode::Onboard {
        onboard.set_mode(OnboardMode::Onboard).await?;
    }
    if onboard.get_current_profile().await? != Some(index) {
        onboard.set_current_profile(index).await?;
    }
    Ok(())
}

// Ownership is recorded before the first write so a failure can roll back.
pub(super) async fn arm_onboard(
    device: &Device,
    chan: &Arc<HidppChannel>,
    device_index: u8,
    target: &OnboardTarget,
    armed: &mut Option<ArmedOnboard>,
) -> Result<(), CaptureError> {
    let root = device.root();
    let Some(onboard_info) = root.get_feature(OnboardProfilesFeature::ID).await? else {
        debug!("onboard target on a device without 0x8100 — ignored");
        return Ok(());
    };
    let mode = match target {
        OnboardTarget::Profile(index) => ArmedMode::Profile(*index),
        OnboardTarget::Host(host) => {
            let Some(spy_info) = root.get_feature(MouseButtonSpyFeature::ID).await? else {
                debug!("host mode requested on a device without 0x8110 — ignored");
                return Ok(());
            };
            let rate_index = root
                .get_feature(ReportRateFeature::ID)
                .await?
                .map(|info| info.index);
            ArmedMode::Host {
                spy_index: spy_info.index,
                rate_index,
                captured: host.captured().collect(),
                host: host.clone(),
            }
        }
    };
    let state = armed.insert(ArmedOnboard {
        chan: Arc::clone(chan),
        device_index,
        onboard_index: onboard_info.index,
        mode,
    });
    let onboard = state.onboard();
    match &state.mode {
        ArmedMode::Host {
            spy_index,
            rate_index,
            host,
            captured,
        } => {
            state
                .apply_host(&onboard, *spy_index, *rate_index, host)
                .await?;
            info!(captured = captured.len(), "G-series host mode active");
        }
        ArmedMode::Profile(index) => apply_profile(&onboard, *index).await?,
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct SpyRestore {
    onboard_index: u8,
    spy_index: u8,
}

impl SpyRestore {
    pub(crate) async fn restore_on(&self, chan: &Arc<HidppChannel>, device_index: u8) -> bool {
        let spy = MouseButtonSpyFeature::new(Arc::clone(chan), device_index, self.spy_index);
        let onboard =
            OnboardProfilesFeature::new(Arc::clone(chan), device_index, self.onboard_index);
        if let Err(error) = spy.stop_spy().await {
            debug!(?error, "stopping 0x8110 spy failed");
        }
        match onboard.set_mode(OnboardMode::Onboard).await {
            Ok(()) => true,
            Err(error) => {
                warn!(?error, "leaving G-series host mode failed");
                false
            }
        }
    }
}

pub(super) fn decode_mask(msg: &v20::Message, device_index: u8, spy_index: u8) -> Option<u16> {
    let header = msg.header();
    if header.device_index != device_index
        || header.feature_index != spy_index
        || header.software_id.to_lo() != 0
    {
        return None;
    }
    match MouseButtonSpyEvent::decode(header.function_id.to_lo(), &msg.extend_payload())? {
        MouseButtonSpyEvent::Buttons { mask } => Some(mask),
        _ => None,
    }
}

#[derive(Default)]
pub(super) struct SpyEdges {
    held: u16,
    sink: Option<mpsc::UnboundedSender<CapturedInput>>,
}

impl SpyEdges {
    /// Where [`Self::release_all`] sends its releases.
    pub(super) fn attach(&mut self, sink: mpsc::UnboundedSender<CapturedInput>) {
        self.sink = Some(sink);
    }

    /// Release every held button. A mouse that power-cycled reports them up
    /// only as an unchanged mask, which is no edge.
    pub(super) fn release_all(&mut self, captured: &[(u8, ButtonId)]) {
        if let Some(sink) = self.sink.clone() {
            self.on_mask(0, captured, &sink);
        }
        self.held = 0;
    }

    pub(super) fn on_mask(
        &mut self,
        mask: u16,
        captured: &[(u8, ButtonId)],
        sink: &mpsc::UnboundedSender<CapturedInput>,
    ) {
        let changed = mask ^ self.held;
        self.held = mask;
        for &(slot, button) in captured {
            let bit = 1u16 << slot;
            if changed & bit == 0 {
                continue;
            }
            let input = if mask & bit == 0 {
                CapturedInput::ButtonUp(button)
            } else {
                CapturedInput::ButtonDown(button)
            };
            let _ = sink.send(input);
        }
    }
}

#[cfg(test)]
mod tests {
    use hidpp::nibble::U4;

    use super::*;

    fn host() -> HostMode {
        HostMode {
            slots: vec![
                (ButtonId::LeftClick, false),
                (ButtonId::RightClick, false),
                (ButtonId::MiddleClick, false),
                (ButtonId::Back, true),
                (ButtonId::Forward, false),
                (ButtonId::G6, true),
                (ButtonId::WheelTiltRight, false),
            ],
            report_rate: None,
        }
    }

    #[test]
    fn mapping_mutes_captured_slots_and_keeps_native_buttons() {
        assert_eq!(host().mapping(), [1, 2, 3, 0, 5, 0, 0]);
        assert_eq!(
            host().captured().collect::<Vec<_>>(),
            [(3, ButtonId::Back), (5, ButtonId::G6)]
        );
    }

    #[test]
    fn edges_follow_captured_bits_only() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let captured = host().captured().collect::<Vec<_>>();
        let mut edges = SpyEdges::default();
        edges.on_mask(0b10_1001, &captured, &tx); // left, Back, G6 down
        edges.on_mask(0b10_0001, &captured, &tx); // Back up
        edges.on_mask(0, &captured, &tx); // G6 up
        let got: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(
            got,
            [
                CapturedInput::ButtonDown(ButtonId::Back),
                CapturedInput::ButtonDown(ButtonId::G6),
                CapturedInput::ButtonUp(ButtonId::Back),
                CapturedInput::ButtonUp(ButtonId::G6),
            ]
        );
    }

    /// A mouse that power-cycles with G-Shift or DPI shift held never reports
    /// the release, so the reconnect has to end the press.
    #[test]
    fn a_reconnect_releases_the_buttons_still_held() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let captured = host().captured().collect::<Vec<_>>();
        let mut edges = SpyEdges::default();
        edges.attach(tx.clone());
        edges.on_mask(0b10_1000, &captured, &tx); // Back, G6 down
        edges.release_all(&captured);
        edges.on_mask(0, &captured, &tx); // the woken mouse's all-up report
        let got: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(
            got,
            [
                CapturedInput::ButtonDown(ButtonId::Back),
                CapturedInput::ButtonDown(ButtonId::G6),
                CapturedInput::ButtonUp(ButtonId::Back),
                CapturedInput::ButtonUp(ButtonId::G6),
            ]
        );
    }

    #[test]
    fn decodes_only_this_sessions_spy_reports() {
        let msg = |device_index, feature_index, software_id| {
            let mut payload = [0; 16];
            payload[..2].copy_from_slice(&[0x01, 0x00]);
            v20::Message::Long(
                v20::MessageHeader {
                    device_index,
                    feature_index,
                    function_id: U4::from_lo(0),
                    software_id: U4::from_lo(software_id),
                },
                payload,
            )
        };
        assert_eq!(decode_mask(&msg(1, 10, 0), 1, 10), Some(0x0100));
        assert_eq!(decode_mask(&msg(2, 10, 0), 1, 10), None);
        assert_eq!(decode_mask(&msg(1, 9, 0), 1, 10), None);
        assert_eq!(decode_mask(&msg(1, 10, 3), 1, 10), None);
    }
}
