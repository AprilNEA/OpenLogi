//! G-series onboard memory, and the bindings a G700 holds in its flash.

use std::sync::Arc;
use std::time::Duration;

use gpui::App;
use openlogi_core::binding::{FlashBindingsState, FlashWrite, GamingButtons, GamingLayout};
use openlogi_core::config::OnboardMemory;
use openlogi_core::hid::{
    Dpi, FlashProfileEdit, FlashProfiles, OnboardMode, OnboardState, ProfileIndex,
};
use tracing::{debug, warn};

use super::device_key::DeviceKey;
use super::events::StateEvents;
use super::load::{FlashProfilesLoad, Load, OnboardLoad};
use super::{AppState, StateEvent};
use crate::state::devices::DeviceRecord;

/// A copy of a flash profile asked of the mouse, until its new page shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingCopy {
    /// The page of the profile copied.
    from: u8,
    /// The pages listed and stored unlisted when the copy was asked: the
    /// copy lands on none of them, but on an erased page.
    known: Vec<u8>,
}

impl PendingCopy {
    /// The copy asked of `profile` on the mouse whose profiles read `profiles`.
    pub(super) fn of(profiles: &FlashProfiles, profile: ProfileIndex) -> Option<Self> {
        Some(Self {
            from: profiles.page_of(profile)?,
            known: profiles
                .pages
                .iter()
                .copied()
                .chain(profiles.disabled.iter().map(|disabled| disabled.page))
                .collect(),
        })
    }

    /// The page the copy landed on, as `profiles` lists it: a page listed
    /// now that was neither listed nor stored before.
    fn landed(&self, profiles: &FlashProfiles) -> Option<u8> {
        let mut new = profiles
            .pages
            .iter()
            .filter(|page| !self.known.contains(page));
        match (new.next(), new.next()) {
            (Some(&page), None) => Some(page),
            _ => None,
        }
    }
}

impl AppState {
    #[must_use]
    pub fn current_onboard_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.onboard_profiles)
    }

    #[must_use]
    pub fn current_gaming_layout(&self) -> Option<&'static GamingLayout> {
        self.current_record()
            .and_then(|record| GamingLayout::for_model_key(&record.model_key))
    }

    /// The buttons the Buttons panel draws for a gaming mouse: its model's
    /// layout, or the standard buttons for a model nobody has dumped.
    #[must_use]
    pub fn current_button_layout(&self) -> Option<&'static GamingLayout> {
        self.current_record()
            .and_then(|record| {
                GamingButtons::for_device(&record.model_key, record.capabilities.as_ref())
            })
            .map(GamingButtons::layout)
    }

    /// The layout of the selected mouse if its flash holds its bindings
    /// (the G700): the agent writes them there.
    #[must_use]
    pub fn current_flash_layout(&self) -> Option<&'static GamingLayout> {
        self.current_record()
            .and_then(|record| {
                GamingButtons::for_device(&record.model_key, record.capabilities.as_ref())
            })
            .and_then(GamingButtons::flash_layout)
    }

    /// What the selected flash-bindings mouse holds, as the agent last read
    /// or wrote it, and where a write stands; `None` until read.
    #[must_use]
    pub fn current_flash_bindings(&self) -> Option<Arc<FlashBindingsState>> {
        let key = self.current_record()?.device_key();
        match self.pointer.reads.flash_bindings_load(&key)? {
            Load::Ready(reading) => Some(Arc::clone(reading)),
            _ => None,
        }
    }

    /// Read the selected flash-bindings mouse again once the agent has had
    /// time to take a binding change, which it then writes to the flash; the
    /// read repeats itself while that write is queued or running.
    pub(crate) fn refresh_flash_bindings_later(cx: &mut App) {
        const WAITS: [Duration; 2] = [Duration::from_millis(500), Duration::from_millis(1500)];
        Self::update(cx, |state, cx| {
            let Some(key) = state
                .current_record()
                .filter(|_| state.current_flash_layout().is_some())
                .map(DeviceRecord::device_key)
            else {
                return;
            };
            cx.spawn(async move |state, cx| {
                for wait in WAITS {
                    cx.background_executor().timer(wait).await;
                    state
                        .update(cx, |state, _| {
                            state.pointer.reads.refresh_flash_bindings(&key);
                        })
                        .ok();
                }
            })
            .detach();
        });
    }

    /// Whether the selected mouse keeps its settings in flash profiles
    /// OpenLogi edits (the G700).
    #[must_use]
    pub fn current_flash_profiles_supported(&self) -> bool {
        self.current_flash_layout().is_some()
    }

    #[must_use]
    pub fn current_flash_profiles_load(&self) -> FlashProfilesLoad {
        self.current_record()
            .and_then(|record| self.pointer.reads.flash_profiles_load(&record.device_key()))
            .cloned()
            .unwrap_or_default()
    }

    /// The selected flash mouse's profiles as last read, if they were.
    #[must_use]
    pub fn current_flash_profiles(&self) -> Option<Arc<FlashProfiles>> {
        match self.current_flash_profiles_load() {
            Load::Ready(reading) => Some(reading),
            _ => None,
        }
    }

    /// Read the selected flash mouse's profiles again — the "click to retry"
    /// affordance on a failed reading.
    pub(crate) fn retry_flash_profiles_read(&mut self, key: &DeviceKey) -> StateEvents {
        self.pointer.reads.refresh_flash_profiles(key);
        StateEvent::OnboardChanged(key.clone()).into()
    }

    /// Why the agent refused the last profile edit asked of the selected
    /// flash mouse.
    #[must_use]
    pub fn current_flash_edit_refusal(&self) -> Option<&str> {
        let key = self.current_record()?.device_key();
        self.pointer
            .flash_edit_refusals
            .get(&key)
            .map(String::as_str)
    }

    /// The DPI presets the selected flash mouse plays: its played profile's
    /// modes, in order. `None` for any other device, whose presets live in
    /// configuration.
    #[must_use]
    pub(crate) fn current_flash_dpi_modes(&self) -> Option<Vec<Dpi>> {
        if !self.current_flash_profiles_supported() {
            return None;
        }
        Some(
            self.current_flash_profiles()
                .and_then(|profiles| {
                    profiles
                        .active
                        .as_ref()
                        .map(openlogi_core::hid::FlashProfileSettings::enabled_dpi_modes)
                })
                .unwrap_or_default(),
        )
    }

    /// How many DPI modes the selected flash mouse's played profile holds,
    /// once its profiles are read. `None` for any other device.
    #[must_use]
    pub(crate) fn current_flash_dpi_slots(&self) -> Option<usize> {
        let profiles = self.current_flash_profiles()?;
        Some(profiles.active.as_ref()?.dpi_modes.len())
    }

    /// Ask the agent to apply `edit` to the selected flash mouse, then read
    /// its profiles again as the edit lands: once the answer is in, and again
    /// while a flash write it queued runs.
    pub(crate) fn edit_flash_profiles(cx: &mut App, edit: FlashProfileEdit) {
        Self::update(cx, |state, cx| {
            let Some((key, route)) = state
                .current_record()
                .filter(|_| state.current_flash_profiles_supported())
                .and_then(|record| Some((record.device_key(), record.route.clone()?)))
            else {
                debug!("no reachable flash mouse selected — profile edit dropped");
                return;
            };
            state.pointer.flash_edit_refusals.remove(&key);
            // A copy takes its source's OpenLogi bindings once it lands; any
            // other edit leaves no copy waiting.
            let copy = match &edit {
                FlashProfileEdit::Duplicate(profile) => state
                    .current_flash_profiles()
                    .and_then(|profiles| PendingCopy::of(&profiles, *profile)),
                _ => None,
            };
            match copy {
                Some(copy) => state.pointer.flash_copies.insert(key.clone(), copy),
                None => state.pointer.flash_copies.remove(&key),
            };
            let (reply, answer) = tokio::sync::oneshot::channel();
            if !state.send_ipc(crate::services::ipc::EditFlashProfiles { route, edit, reply }) {
                return;
            }
            cx.spawn(async move |state, cx| {
                let result = answer
                    .await
                    .unwrap_or(Err(openlogi_core::hid::WriteError::AgentUnavailable));
                state
                    .update(cx, |state, cx| {
                        if let Err(error) = result {
                            warn!(%error, "the agent refused a profile edit");
                            state.pointer.flash_copies.remove(&key);
                            state
                                .pointer
                                .flash_edit_refusals
                                .insert(key.clone(), error.to_string());
                        }
                        let reads = &mut state.pointer.reads;
                        reads.refresh_flash_profiles(&key);
                        reads.refresh_flash_bindings(&key);
                        cx.emit(StateEvent::OnboardChanged(key));
                    })
                    .ok();
            })
            .detach();
        });
    }

    /// Make the profile on `page`, which the flash mouse `key` was just
    /// found playing, the one whose bindings the Buttons panel edits
    /// ([`openlogi_core::config::Config::select_flash_profile`]). Returns
    /// whether the edited profile changed.
    pub(crate) fn follow_flash_profile(&mut self, key: &DeviceKey, page: u8) -> bool {
        let Some(config_key) = self.flash_config_key(key) else {
            return false;
        };
        let selected = self
            .config
            .devices
            .get(&config_key)
            .and_then(|device| device.flash_page);
        if selected == Some(page) {
            return false;
        }
        self.config
            .edit(|config| config.select_flash_profile(&config_key, page));
        self.refresh_binding_projections();
        self.persist_and_reload("flash profile selection");
        true
    }

    /// Give a profile copy that landed, as `profiles` — the mouse `key`'s
    /// latest reading — shows it, the OpenLogi bindings of the profile it
    /// copied. A copy whose write failed is forgotten; one still writing
    /// waits.
    pub(crate) fn settle_flash_copy(&mut self, key: &DeviceKey, profiles: &FlashProfiles) {
        let Some(copy) = self.pointer.flash_copies.get(key) else {
            return;
        };
        match &profiles.write {
            FlashWrite::Pending | FlashWrite::Writing => return,
            FlashWrite::Failed(_) => {
                self.pointer.flash_copies.remove(key);
                return;
            }
            FlashWrite::Idle => {}
        }
        let Some(to) = copy.landed(profiles) else {
            return;
        };
        let from = copy.from;
        self.pointer.flash_copies.remove(key);
        let Some(config_key) = self.flash_config_key(key) else {
            return;
        };
        if self
            .config
            .edit(|config| config.copy_flash_profile(&config_key, from, to))
        {
            self.persist_and_reload("flash profile copy");
        }
    }

    /// The configuration key of the flash mouse `key`, when settings may be
    /// stored for it.
    fn flash_config_key(&self, key: &DeviceKey) -> Option<String> {
        self.devices()
            .iter()
            .find(|record| record.device_key() == *key)
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_owned)
    }

    pub(super) fn load_current_flash_profiles(&mut self, cx: &mut gpui::Context<Self>) {
        if !self.current_flash_profiles_supported() {
            return;
        }
        let Some((key, route)) = self
            .current_record()
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.pointer
            .reads
            .ensure_flash_profiles(key, route, self.ipc_sender(), cx);
    }

    #[must_use]
    pub fn current_onboard_load(&self) -> OnboardLoad {
        self.current_record()
            .and_then(|record| self.pointer.reads.onboard_load(&record.device_key()))
            .cloned()
            .unwrap_or_default()
    }

    #[must_use]
    pub fn current_onboard_memory(&self) -> Option<OnboardMemory> {
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)?;
        self.config
            .effective_onboard_memory(key, self.current_gaming_layout())
    }

    /// Who controls the mouse now, which is not always who config asked for:
    /// a host setup the mouse rejected leaves it on its own profile.
    #[must_use]
    pub fn current_onboard_active(&self) -> Option<OnboardMemory> {
        let reading = match self.current_onboard_load() {
            Load::Ready(reading) => Some(reading),
            _ => None,
        };
        active_memory(reading.as_deref(), self.current_onboard_memory())
    }

    /// Whether a G-series mouse's stored profile owns its report rate: its
    /// `0x8060` takes a write only while OpenLogi controls the mouse.
    #[must_use]
    pub fn current_report_rate_locked(&self) -> bool {
        self.current_onboard_supported()
            && self.current_onboard_active() != Some(OnboardMemory::Off)
    }

    pub fn commit_onboard_memory(&mut self, memory: OnboardMemory) -> StateEvents {
        let events = self.for_current_device(StateEvent::OnboardChanged);
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return events;
        };
        self.config
            .edit(|config| config.set_onboard_memory(&key, Some(memory)));
        self.persist_and_reload("onboard memory");
        events
    }

    /// Re-read the active G-series mouse now and once more after the agent has
    /// settled, for a change made outside the Pointer tab: a binding edit can
    /// move the mouse in or out of host mode.
    pub(crate) fn refresh_onboard(cx: &mut App) {
        let supported = Self::update(cx, |state, _| {
            let key = state
                .current_record()
                .filter(|_| state.current_onboard_supported())
                .map(DeviceRecord::device_key)?;
            state.pointer.reads.refresh_onboard(&key);
            Some(())
        });
        if supported.is_some() {
            Self::refresh_onboard_later(cx);
        }
    }

    /// Re-read the active mouse while the agent applies a change: early for
    /// the usual case, and again once it has had time to settle.
    pub(crate) fn refresh_onboard_later(cx: &mut App) {
        const WAITS: [Duration; 2] = [Duration::from_millis(700), Duration::from_millis(1300)];
        Self::update(cx, |state, cx| {
            let Some(key) = state.current_record().map(DeviceRecord::device_key) else {
                return;
            };
            cx.spawn(async move |state, cx| {
                for wait in WAITS {
                    cx.background_executor().timer(wait).await;
                    state
                        .update(cx, |state, _| {
                            state.pointer.reads.refresh_onboard(&key);
                            // A profile switch also changes the DPI and report rate.
                            state.pointer.reads.retry_dpi(&key);
                            state.pointer.reads.retry_report_rate(&key);
                        })
                        .ok();
                }
            })
            .detach();
        });
    }

    pub(super) fn load_current_flash_bindings(&mut self, cx: &mut gpui::Context<Self>) {
        if self.current_flash_layout().is_none() {
            return;
        }
        let Some((key, route)) = self
            .current_record()
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.pointer
            .reads
            .ensure_flash_bindings(key, route, self.ipc_sender(), cx);
    }

    pub(super) fn load_current_onboard(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| {
                record
                    .capabilities
                    .is_some_and(|capabilities| capabilities.onboard_profiles)
            })
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.pointer
            .reads
            .ensure_onboard(key, route, self.ipc_sender(), cx);
    }
}

/// The mode the mouse reports, or `requested` until it has been read.
fn active_memory(
    reading: Option<&OnboardState>,
    requested: Option<OnboardMemory>,
) -> Option<OnboardMemory> {
    reading
        .map(|reading| match (reading.mode, reading.active_profile) {
            (OnboardMode::Onboard, Some(index)) => OnboardMemory::Profile(index),
            _ => OnboardMemory::Off,
        })
        .or(requested)
}

#[cfg(test)]
mod tests {
    use openlogi_core::hid::ProfileIndex;

    use super::*;

    fn profile(index: u8) -> ProfileIndex {
        ProfileIndex::new(index).unwrap()
    }

    fn reading(mode: OnboardMode, active_profile: Option<ProfileIndex>) -> OnboardState {
        OnboardState {
            mode,
            active_profile,
            profiles: Vec::new(),
        }
    }

    /// Host mode was requested, but the mouse rejected it and is back on a
    /// profile: the card must not claim OpenLogi is in control.
    #[test]
    fn the_mouse_reading_wins_over_the_requested_mode() {
        let on_profile = reading(OnboardMode::Onboard, Some(profile(5)));
        assert_eq!(
            active_memory(Some(&on_profile), Some(OnboardMemory::Off)),
            Some(OnboardMemory::Profile(profile(5)))
        );
        let in_host = reading(OnboardMode::Host, None);
        assert_eq!(
            active_memory(Some(&in_host), Some(OnboardMemory::Profile(profile(1)))),
            Some(OnboardMemory::Off)
        );
    }

    #[test]
    fn the_requested_mode_stands_in_until_the_mouse_is_read() {
        assert_eq!(
            active_memory(None, Some(OnboardMemory::Profile(profile(2)))),
            Some(OnboardMemory::Profile(profile(2)))
        );
        assert_eq!(active_memory(None, None), None);
    }
}
