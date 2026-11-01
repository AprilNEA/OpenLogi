//! A scripted Logitech G700 behind its Nano receiver, for exercising the
//! flash-profile and flash-binding panels without the mouse.
//!
//! Its data is the user's mouse as dumped on 2026-10-31: three enabled
//! profiles, two of them named by a tool that kept only the low byte of each
//! UTF-16 unit (so they read `"\u{1f}@>D8;L 1"`), the third played, its
//! DPI modes, rates and bindings, and a full battery. Edits change this
//! state the way the agent's flash writes change the mouse — a copy lands on
//! the first free profile page, a disabled profile keeps its page — and the
//! write reports [`FlashWrite::Writing`] for a moment, as a page rewrite
//! does.

use std::time::Duration;

use openlogi_core::binding::{FlashBinding, FlashBindingsState, FlashWrite, G700};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, DeviceInventory, DeviceKind,
    PairedDevice, ReceiverInfo,
};
use openlogi_core::hid::{
    DeviceRoute, DisabledProfile, Dpi, DpiCapabilities, DpiInfo, FlashProfileEdit,
    FlashProfileSettings, FlashProfiles, OnboardMode, OnboardProfile, OnboardState, ProfileIndex,
    ReportRate, ReportRateInfo, WriteError,
};

/// Unique ID of the scripted Nano receiver.
const RECEIVER_UID: &str = "OL-C531-UID-0001";
const SLOT: u8 = 1;
/// How long a scripted flash write reports itself running.
const WRITE_TIME: Duration = Duration::from_millis(1500);
/// The profile pages a new profile may take, as the agent's device layer
/// has them: 2 to 4 and 8 to 24.
const PROFILE_PAGES: [u8; 20] = [
    2, 3, 4, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
];
const SLOTS: usize = 5;

/// One stored profile.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Profile {
    page: u8,
    name: String,
    dpi_modes: [Option<Dpi>; 5],
    /// The start-mode byte as stored; `0x82` on two of the dumped profiles.
    default_mode: u8,
    rate: ReportRate,
    bindings: Vec<FlashBinding>,
}

/// The scripted mouse.
#[derive(Debug)]
pub(super) struct MockG700 {
    /// The directory, in slot order.
    listed: Vec<Profile>,
    /// Profiles stored on pages the directory does not list.
    unlisted: Vec<Profile>,
    /// Directory position of the played profile.
    played: usize,
    live_dpi: Dpi,
    live_rate: ReportRate,
    /// When the scripted write in progress ends, on the mock's clock.
    writing_until: Option<Duration>,
    /// The error the last edit ended with.
    failure: Option<String>,
}

fn dpi_modes(values: &[u16]) -> [Option<Dpi>; 5] {
    std::array::from_fn(|mode| values.get(mode).map(|&dpi| Dpi::new(dpi)))
}

fn button(n: u8) -> FlashBinding {
    FlashBinding::MouseButtons(1 << (n - 1))
}

impl MockG700 {
    /// The mouse as dumped.
    pub(super) fn dumped() -> Self {
        let first = G700.flash_defaults.to_vec();
        let mut third = first.clone();
        third[3..7].copy_from_slice(&[button(7), button(6), button(4), button(5)]);
        third[8] = button(11);
        third[9] = button(10);
        Self {
            listed: vec![
                Profile {
                    page: 2,
                    name: "\u{1f}@>D8;L 1".into(),
                    dpi_modes: dpi_modes(&[400, 800, 1200, 1600, 3200]),
                    default_mode: 0x82,
                    rate: ReportRate::new(500),
                    bindings: first.clone(),
                },
                Profile {
                    page: 3,
                    name: "Профиль 2".into(),
                    dpi_modes: dpi_modes(&[400, 1600]),
                    default_mode: 1,
                    rate: ReportRate::new(1000),
                    bindings: first,
                },
                Profile {
                    page: 4,
                    name: "\u{1f}@>D8;L 3".into(),
                    dpi_modes: dpi_modes(&[850, 1600, 2100]),
                    default_mode: 0,
                    rate: ReportRate::new(250),
                    bindings: third,
                },
            ],
            unlisted: Vec::new(),
            played: 2,
            live_dpi: Dpi::new(850),
            live_rate: ReportRate::new(250),
            writing_until: None,
            failure: None,
        }
    }

    /// The Nano receiver with the mouse in its first slot.
    pub(super) fn inventory() -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "USB Receiver".into(),
                vendor_id: 0x046d,
                product_id: 0xc531,
                unique_id: Some(RECEIVER_UID.into()),
            },
            paired: vec![PairedDevice {
                slot: SLOT,
                codename: Some("G700".into()),
                wpid: None,
                kind: DeviceKind::Mouse,
                online: true,
                battery: Some(BatteryInfo {
                    percentage: None,
                    level: BatteryLevel::Full,
                    status: BatteryStatus::Discharging,
                }),
                model_info: None,
                capabilities: Some(Capabilities {
                    pointer: true,
                    report_rate: true,
                    flash_bindings: true,
                    ..Capabilities::from_feature_ids(&[])
                }),
                settings_revision: 0,
                hidpp10: true,
            }],
        }
    }

    /// Whether `route` is the scripted mouse.
    pub(super) fn is_route(route: &DeviceRoute) -> bool {
        DeviceRoute::for_slot(&Self::inventory(), SLOT).as_ref() == Some(route)
    }

    fn write_state(&self, now: Duration) -> FlashWrite {
        match (self.writing_until, &self.failure) {
            (Some(until), _) if now < until => FlashWrite::Writing,
            (_, Some(error)) => FlashWrite::Failed(error.clone()),
            _ => FlashWrite::Idle,
        }
    }

    fn played_profile(&self) -> &Profile {
        &self.listed[self.played]
    }

    pub(super) fn dpi(&self) -> Result<DpiInfo, WriteError> {
        Ok(DpiInfo {
            current: self.live_dpi,
            capabilities: dpi_capabilities()?,
        })
    }

    /// The live DPI, saved to the played profile's mode that held the one
    /// before, as the agent saves it.
    pub(super) fn set_dpi(&mut self, dpi: Dpi, now: Duration) -> Result<(), WriteError> {
        let dpi = dpi_capabilities()?.nearest(dpi);
        let from = self.live_dpi;
        self.live_dpi = dpi;
        let played = self.played;
        if let Some(mode) = self.listed[played]
            .dpi_modes
            .iter_mut()
            .find(|mode| **mode == Some(from))
        {
            *mode = Some(dpi);
            self.wrote(now);
        }
        Ok(())
    }

    pub(super) fn report_rate(&self) -> ReportRateInfo {
        ReportRateInfo {
            current: self.live_rate,
            supported: [125, 250, 500, 1000].map(ReportRate::new).to_vec(),
        }
    }

    pub(super) fn set_report_rate(
        &mut self,
        rate: ReportRate,
        now: Duration,
    ) -> Result<ReportRateInfo, WriteError> {
        if !self.report_rate().supports(rate) {
            return Err(WriteError::ReportRateUnsupported { requested: rate });
        }
        self.live_rate = rate;
        let played = self.played;
        self.listed[played].rate = rate;
        self.wrote(now);
        Ok(self.report_rate())
    }

    pub(super) fn bindings(&self, now: Duration) -> FlashBindingsState {
        FlashBindingsState {
            profile: u8::try_from(self.played).ok(),
            slots: self.played_profile().bindings.clone(),
            write: self.write_state(now),
            ..FlashBindingsState::default()
        }
    }

    pub(super) fn profiles(&self, now: Duration) -> FlashProfiles {
        let profiles = (0..SLOTS)
            .filter_map(|position| {
                let index = ProfileIndex::new(u8::try_from(position + 1).ok()?)?;
                let listed = self.listed.get(position);
                Some(OnboardProfile {
                    index,
                    name: listed.map(|profile| profile.name.clone()),
                    enabled: listed.is_some(),
                })
            })
            .collect();
        let played = self.played_profile();
        FlashProfiles {
            onboard: OnboardState {
                mode: OnboardMode::Onboard,
                active_profile: u8::try_from(self.played + 1)
                    .ok()
                    .and_then(ProfileIndex::new),
                profiles,
            },
            active: Some(FlashProfileSettings {
                dpi_modes: played.dpi_modes.to_vec(),
                default_mode: (played.default_mode < 5).then_some(played.default_mode),
                report_rate: Some(played.rate),
            }),
            disabled: self
                .unlisted
                .iter()
                .filter(|profile| PROFILE_PAGES.contains(&profile.page))
                .map(|profile| DisabledProfile {
                    page: profile.page,
                    name: Some(profile.name.clone()),
                })
                .collect(),
            can_create: self.free_page().is_some(),
            write: self.write_state(now),
        }
    }

    fn free_page(&self) -> Option<u8> {
        PROFILE_PAGES.into_iter().find(|page| {
            !self
                .listed
                .iter()
                .chain(&self.unlisted)
                .any(|profile| profile.page == *page)
        })
    }

    fn wrote(&mut self, now: Duration) {
        self.writing_until = Some(now + WRITE_TIME);
        self.failure = None;
    }

    /// Apply `edit` as the agent would: a selection at once, the rest as a
    /// flash write that reports itself for [`WRITE_TIME`]. A refusal the
    /// agent makes before queueing is an error; one it meets while writing
    /// is the write's failure.
    pub(super) fn edit(&mut self, edit: FlashProfileEdit, now: Duration) -> Result<(), WriteError> {
        let position = |profile: ProfileIndex| usize::from(profile.get().get() - 1);
        match edit {
            FlashProfileEdit::Activate(profile) => {
                let position = position(profile);
                if position >= self.listed.len() {
                    return Err(WriteError::Hidpp(format!(
                        "profile {} is not enabled: the directory lists {}",
                        position,
                        self.listed.len()
                    )));
                }
                self.played = position;
                let played = self.played_profile();
                let rate = played.rate;
                let dpi = played
                    .dpi_modes
                    .get(usize::from(played.default_mode))
                    .copied()
                    .flatten()
                    .or_else(|| played.dpi_modes.iter().flatten().next().copied());
                self.live_rate = rate;
                if let Some(dpi) = dpi {
                    self.live_dpi = dpi;
                }
                return Ok(());
            }
            FlashProfileEdit::Rename { ref name, .. } if name.encode_utf16().count() > 23 => {
                return Err(WriteError::Hidpp(
                    "the profile name cannot be stored: the name needs more than 23 UTF-16 units"
                        .into(),
                ));
            }
            FlashProfileEdit::DpiModes(ref modes) if modes.is_empty() || modes.len() > 5 => {
                return Err(WriteError::Hidpp(format!(
                    "a profile keeps one to 5 DPI modes, not {}",
                    modes.len()
                )));
            }
            _ => {}
        }
        let result = self.flash_edit(edit);
        self.wrote(now);
        if let Err(error) = result {
            self.failure = Some(error);
        }
        Ok(())
    }

    fn flash_edit(&mut self, edit: FlashProfileEdit) -> Result<(), String> {
        let slot = |profile: ProfileIndex| usize::from(profile.get().get() - 1);
        match edit {
            FlashProfileEdit::Activate(_) => Err("selecting a profile is not a flash edit".into()),
            FlashProfileEdit::Rename { profile, name } => {
                let stored = self
                    .listed
                    .get_mut(slot(profile))
                    .ok_or("no such profile")?;
                stored.name = name;
                Ok(())
            }
            FlashProfileEdit::DpiModes(modes) => {
                let played = self.played;
                let stored = &mut self.listed[played];
                stored.dpi_modes = std::array::from_fn(|mode| modes.get(mode).copied());
                if usize::from(stored.default_mode) < 5
                    && usize::from(stored.default_mode) >= modes.len()
                {
                    stored.default_mode = 0;
                }
                Ok(())
            }
            FlashProfileEdit::Disable(profile) => {
                let position = slot(profile);
                if self.listed.len() <= 1 {
                    return Err("the last enabled profile cannot be disabled".into());
                }
                if position >= self.listed.len() {
                    return Err("no such profile".into());
                }
                let removed = self.listed.remove(position);
                self.unlisted.push(removed);
                self.unlisted.sort_by_key(|profile| profile.page);
                if self.played >= position && self.played > 0 {
                    self.played -= 1;
                }
                Ok(())
            }
            FlashProfileEdit::Enable { page } => {
                if self.listed.len() >= SLOTS {
                    return Err("all profile slots are taken".into());
                }
                let at = self
                    .unlisted
                    .iter()
                    .position(|profile| profile.page == page)
                    .ok_or_else(|| format!("page {page} holds no profile that can be enabled"))?;
                let profile = self.unlisted.remove(at);
                self.listed.push(profile);
                Ok(())
            }
            FlashProfileEdit::Duplicate(profile) => {
                if self.listed.len() >= SLOTS {
                    return Err("all profile slots are taken".into());
                }
                let mut copy = self
                    .listed
                    .get(slot(profile))
                    .ok_or("no such profile")?
                    .clone();
                copy.page = self
                    .free_page()
                    .ok_or("no free memory page is left for a new profile")?;
                self.listed.push(copy);
                Ok(())
            }
        }
    }
}

fn dpi_capabilities() -> Result<DpiCapabilities, WriteError> {
    DpiCapabilities::new((1..=228).map(|step| step * 25).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(n: u8) -> ProfileIndex {
        ProfileIndex::new(n).expect("a profile number")
    }

    const NOW: Duration = Duration::ZERO;
    const LATER: Duration = Duration::from_secs(10);

    #[test]
    fn the_dumped_mouse_lists_three_profiles_and_plays_the_third() {
        let mouse = MockG700::dumped();
        let listing = mouse.profiles(NOW);
        assert_eq!(listing.enabled_count(), 3);
        assert_eq!(listing.onboard.profiles.len(), 5);
        assert_eq!(listing.onboard.active_profile, Some(profile(3)));
        let active = listing.active.expect("the played profile's settings");
        assert_eq!(active.enabled_dpi_modes(), [850, 1600, 2100].map(Dpi::new));
        assert_eq!(active.report_rate, Some(ReportRate::new(250)));
        assert!(listing.can_create && listing.disabled.is_empty());
        assert_eq!(listing.write, FlashWrite::Idle);
        assert_eq!(mouse.bindings(NOW).slots[8], button(11));
    }

    #[test]
    fn edits_change_the_flash_and_report_a_write_for_a_moment() {
        let mut mouse = MockG700::dumped();
        mouse
            .edit(FlashProfileEdit::Duplicate(profile(2)), NOW)
            .expect("queued");
        assert_eq!(mouse.profiles(NOW).write, FlashWrite::Writing);
        let listing = mouse.profiles(LATER);
        assert_eq!(listing.write, FlashWrite::Idle);
        assert_eq!(listing.enabled_count(), 4);
        assert_eq!(mouse.listed[3].page, 8, "the first free profile page");

        mouse
            .edit(FlashProfileEdit::Disable(profile(1)), NOW)
            .expect("queued");
        assert_eq!(mouse.played, 1, "the played profile moved up and plays on");
        let listing = mouse.profiles(LATER);
        assert_eq!(listing.disabled.len(), 1);
        mouse
            .edit(FlashProfileEdit::Enable { page: 2 }, NOW)
            .expect("queued");
        assert_eq!(mouse.profiles(LATER).enabled_count(), 4);
    }

    #[test]
    fn the_last_profile_stays_and_its_refusal_is_the_writes_failure() {
        let mut mouse = MockG700::dumped();
        for _ in 0..2 {
            mouse
                .edit(FlashProfileEdit::Disable(profile(1)), NOW)
                .expect("queued");
        }
        mouse
            .edit(FlashProfileEdit::Disable(profile(1)), NOW)
            .expect("queued");
        assert_eq!(mouse.listed.len(), 1);
        assert!(matches!(mouse.profiles(LATER).write, FlashWrite::Failed(_)));
    }

    #[test]
    fn a_slider_move_saves_to_the_mode_the_mouse_was_in() {
        let mut mouse = MockG700::dumped();
        mouse.set_dpi(Dpi::new(1000), NOW).expect("set");
        mouse.set_dpi(Dpi::new(1100), NOW).expect("set");
        let listing = mouse.profiles(LATER);
        assert_eq!(
            listing.active.expect("settings").enabled_dpi_modes(),
            [1100, 1600, 2100].map(Dpi::new)
        );
        assert_eq!(mouse.dpi().expect("dpi").current, Dpi::new(1100));
    }
}
