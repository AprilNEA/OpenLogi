//! Onboard-profile controls for devices with HID++ feature `0x8100`.
//!
//! The panel selects whether OpenLogi or onboard memory drives the device and,
//! in onboard mode, which writable profile is active. Reads are lazy and every
//! write is followed by a confirming read so rejected changes do not remain as
//! optimistic UI state.

use gpui::{
    AnyElement, App, BorrowAppContext as _, Context, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div,
};
use gpui_component::{Selectable as _, button::Button, h_flex, v_flex};
use openlogi_core::hid::{DeviceRoute, OnboardProfilesInfo, ProfileEntry, ProfilesMode};

use crate::state::{AppState, DeviceKey, ProfilesLoad};
use crate::ui::device_read::issue_device_read;
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, Typography as _};

/// Onboard-profile source and active-profile controls.
pub struct ProfilesPanel {
    _state_obs: Subscription,
}

impl ProfilesPanel {
    /// Construct the panel and subscribe it to application-state changes.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = cx.observe_global::<AppState>(|_, cx| cx.notify());
        Self {
            _state_obs: state_obs,
        }
    }

    fn ensure_profiles_load(cx: &mut Context<Self>) {
        let Some((key, route)) = profiles_load_target(cx) else {
            return;
        };
        cx.update_global::<AppState, _>(|state, _| state.reads.profiles.mark_loading(&key));
        Self::issue_profiles_read(
            key,
            route,
            |state, key| state.reads.profiles.clear_loading(key),
            cx,
        );
    }

    fn ensure_profiles_confirm(cx: &mut Context<Self>) {
        let Some((key, route)) =
            cx.update_global::<AppState, _>(|state, _| state.take_active_profiles_confirm())
        else {
            return;
        };
        Self::issue_profiles_read(key, route, |_, _| {}, cx);
    }

    fn issue_profiles_read(
        key: DeviceKey,
        route: DeviceRoute,
        clear: impl Fn(&mut AppState, &DeviceKey) + 'static,
        cx: &mut Context<Self>,
    ) {
        issue_device_read(
            cx,
            key,
            route,
            crate::services::ipc::Command::ReadOnboardProfiles,
            AppState::store_profiles_info,
            clear,
        );
    }

    fn ready_body(info: &OnboardProfilesInfo, pal: Palette) -> AnyElement {
        let onboard = info.mode == ProfilesMode::Onboard;
        let keep_profile = keep_profile_for(info);
        let source_row = v_flex()
            .gap_2()
            .child(section_label(tr!("Settings source"), pal))
            .child(
                h_flex()
                    .gap_2()
                    .child(source_button(
                        "profiles-source-host",
                        tr!("OpenLogi settings"),
                        !onboard,
                        ProfilesMode::Host,
                        None,
                    ))
                    .child(source_button(
                        "profiles-source-onboard",
                        tr!("Onboard memory"),
                        onboard,
                        ProfilesMode::Onboard,
                        keep_profile,
                    )),
            )
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(if onboard {
                        tr!(
                            "The mouse runs the profile stored in its memory; OpenLogi settings do not apply."
                        )
                    } else {
                        tr!("OpenLogi drives this mouse; the onboard profile is dormant.")
                    }),
            );

        let mut body = v_flex().gap_4().w_full().child(source_row);
        if onboard {
            let profiles: Vec<_> = selectable_profiles(info).collect();
            let profile_row = v_flex()
                .gap_2()
                .child(section_label(tr!("Active onboard profile"), pal))
                .child(if profiles.is_empty() {
                    status_line(tr!("No enabled profiles in the device's memory."), pal)
                } else {
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .children(profiles.into_iter().map(|(index, entry)| {
                            profile_button(index, entry, entry.sector == info.active_profile)
                        }))
                        .into_any_element()
                });
            body = body.child(profile_row);
        }
        body.into_any_element()
    }
}

impl Render for ProfilesPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Self::ensure_profiles_load(cx);
        Self::ensure_profiles_confirm(cx);
        let pal = theme::palette(cx);
        let (key, status) = cx
            .try_global::<AppState>()
            .and_then(|state| {
                let key = state.current_record()?.device_key();
                Some((Some(key), state.current_profiles_status()))
            })
            .unwrap_or((None, ProfilesLoad::Unknown));
        let reachable = cx
            .try_global::<AppState>()
            .and_then(AppState::current_record)
            .is_some_and(|record| record.route.is_some());

        let content: AnyElement = match status {
            ProfilesLoad::Ready(info) => Self::ready_body(&info, pal),
            ProfilesLoad::Loading | ProfilesLoad::Unknown if !reachable => {
                status_line(tr!("Device offline — onboard profiles unavailable."), pal)
            }
            ProfilesLoad::Loading | ProfilesLoad::Unknown => {
                status_line(tr!("Reading onboard profiles…"), pal)
            }
            ProfilesLoad::Failed(_) => retry_line(
                "profiles-retry",
                tr!("Couldn't read onboard profiles — click to retry."),
                pal,
                retry_profiles_closure(key),
            ),
            ProfilesLoad::Unsupported(_) => {
                status_line(tr!("This device has no onboard profile memory."), pal)
            }
        };

        v_flex().gap_3().w_full().child(content)
    }
}

fn profiles_load_target(cx: &mut Context<ProfilesPanel>) -> Option<(DeviceKey, DeviceRoute)> {
    cx.try_global::<AppState>().and_then(|state| {
        let record = state.current_record()?;
        let key = record.device_key();
        if !state.current_profiles_unqueried() {
            return None;
        }
        Some((key, record.route.clone()?))
    })
}

fn retry_profiles_closure(key: Option<DeviceKey>) -> impl Fn(&mut App) + 'static {
    move |cx| {
        if let Some(key) = &key {
            cx.update_global::<AppState, _>(|state, _| state.retry_profiles(key));
        }
        cx.refresh_windows();
    }
}

/// Keep the active writable profile, or fall back to the first enabled one.
/// ROM sectors are excluded because firmware rejects them as active profiles.
fn keep_profile_for(info: &OnboardProfilesInfo) -> Option<u16> {
    let active_is_selectable =
        selectable_profiles(info).any(|(_, entry)| entry.sector == info.active_profile);
    active_is_selectable
        .then_some(info.active_profile)
        .or_else(|| {
            selectable_profiles(info)
                .next()
                .map(|(_, entry)| entry.sector)
        })
}

/// Selectable profiles paired with their original directory positions.
fn selectable_profiles(
    info: &OnboardProfilesInfo,
) -> impl Iterator<Item = (usize, ProfileEntry)> + '_ {
    info.directory
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, entry)| entry.enabled && !entry.is_rom())
}

fn section_label(text: SharedString, pal: Palette) -> AnyElement {
    div()
        .text_body()
        .text_color(pal.text_muted)
        .child(text)
        .into_any_element()
}

fn source_button(
    id: &'static str,
    label: SharedString,
    selected: bool,
    target: ProfilesMode,
    profile: Option<u16>,
) -> Button {
    Button::new(id)
        .compact()
        .label(label)
        .selected(selected)
        .on_click(move |_, _, cx| {
            cx.update_global::<AppState, _>(|state, _| {
                state.commit_onboard_profiles(target, profile);
            });
            cx.refresh_windows();
        })
}

fn profile_button(index: usize, entry: ProfileEntry, selected: bool) -> Button {
    let sector = entry.sector;
    let n = (index + 1).to_string();
    Button::new(SharedString::from(format!("profile-button-{sector}")))
        .compact()
        .label(tr!("Profile %{n}", n => n))
        .selected(selected)
        .on_click(move |_, _, cx| {
            cx.update_global::<AppState, _>(|state, _| {
                state.commit_onboard_profiles(ProfilesMode::Onboard, Some(sector));
            });
            cx.refresh_windows();
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(active_profile: u16, directory: Vec<ProfileEntry>) -> OnboardProfilesInfo {
        OnboardProfilesInfo {
            profile_count: 3,
            profile_count_oob: 1,
            button_count: 5,
            sector_count: 3,
            sector_size: 256,
            memory_model_id: 1,
            profile_format_id: 1,
            macro_format_id: 1,
            mode: ProfilesMode::Onboard,
            active_profile,
            directory,
        }
    }

    #[test]
    fn selectable_profiles_exclude_disabled_and_rom_entries_without_renumbering() {
        let info = info(
            3,
            vec![
                ProfileEntry {
                    sector: 1,
                    enabled: false,
                },
                ProfileEntry {
                    sector: 0x0101,
                    enabled: true,
                },
                ProfileEntry {
                    sector: 3,
                    enabled: true,
                },
            ],
        );
        let profiles: Vec<_> = selectable_profiles(&info).collect();
        assert_eq!(profiles, vec![(2, info.directory[2])]);
    }

    #[test]
    fn profile_fallback_never_selects_rom() {
        let info = info(
            0x0101,
            vec![
                ProfileEntry {
                    sector: 0x0101,
                    enabled: true,
                },
                ProfileEntry {
                    sector: 2,
                    enabled: true,
                },
            ],
        );
        assert_eq!(keep_profile_for(&info), Some(2));
    }
}
