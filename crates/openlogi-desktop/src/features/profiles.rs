//! Onboard-profile controls for devices with HID++ feature `0x8100`.

use gpui::{
    AnyElement, App, Context, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Window, div,
};
use gpui_component::{Selectable as _, button::Button, h_flex, v_flex};
use openlogi_core::hid::{OnboardProfilesInfo, ProfileEntry, ProfilesMode};

use crate::state::{AppState, DeviceKey, ProfilesLoad, StateEvent};
use crate::ui::section::section_label;
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, Typography as _};

/// Onboard-profile source and active-profile controls.
pub struct ProfilesPanel {
    _state_obs: Subscription,
}

impl ProfilesPanel {
    /// Construct the panel and subscribe it to relevant application-state changes.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = cx.subscribe(&AppState::global(cx), |_, _, event: &StateEvent, cx| {
            let relevant = match event {
                StateEvent::InventoryChanged | StateEvent::DeviceSelected(_) => true,
                StateEvent::ProfilesChanged(key) => AppState::try_read(cx)
                    .and_then(AppState::current_record)
                    .is_some_and(|record| record.device_key() == *key),
                _ => false,
            };
            if relevant {
                cx.notify();
            }
        });
        Self {
            _state_obs: state_obs,
        }
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
            body = body.child(
                v_flex()
                    .gap_2()
                    .child(section_label(tr!("Active onboard profile"), pal))
                    .child(if profiles.is_empty() {
                        status_line(tr!("No enabled profiles in the device's memory."), pal)
                            .into_any_element()
                    } else {
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .children(profiles.into_iter().map(|(index, entry)| {
                                profile_button(index, entry, entry.sector == info.active_profile)
                            }))
                            .into_any_element()
                    }),
            );
        }
        body.into_any_element()
    }
}

impl Render for ProfilesPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let (key, status) = AppState::try_read(cx)
            .and_then(|state| {
                let key = state.current_record()?.device_key();
                Some((Some(key), state.current_profiles_status()))
            })
            .unwrap_or((None, ProfilesLoad::Unknown));
        let reachable = AppState::try_read(cx)
            .and_then(AppState::current_record)
            .is_some_and(|record| record.route.is_some());

        let content = match status {
            ProfilesLoad::Ready(info) => Self::ready_body(&info, pal),
            ProfilesLoad::Loading | ProfilesLoad::Unknown if !reachable => {
                status_line(tr!("Device offline — onboard profiles unavailable."), pal)
                    .into_any_element()
            }
            ProfilesLoad::Loading | ProfilesLoad::Unknown => {
                status_line(tr!("Reading onboard profiles…"), pal).into_any_element()
            }
            ProfilesLoad::Failed(_) => retry_line(
                "profiles-retry",
                tr!("Couldn't read onboard profiles — click to retry."),
                pal,
                retry_profiles_closure(key),
            )
            .into_any_element(),
            ProfilesLoad::Unsupported(_) => {
                status_line(tr!("This device has no onboard profile memory."), pal)
                    .into_any_element()
            }
        };

        v_flex().gap_3().w_full().child(content)
    }
}

fn retry_profiles_closure(key: Option<DeviceKey>) -> impl Fn(&mut App) + 'static {
    move |cx| {
        if let Some(key) = &key {
            AppState::retry_profiles_read(cx, key.clone());
        }
    }
}

/// Keep the active writable profile, or fall back to the first enabled one.
fn keep_profile_for(info: &OnboardProfilesInfo) -> Option<u16> {
    selectable_profiles(info)
        .any(|(_, entry)| entry.sector == info.active_profile)
        .then_some(info.active_profile)
        .or_else(|| {
            selectable_profiles(info)
                .next()
                .map(|(_, entry)| entry.sector)
        })
}

fn selectable_profiles(
    info: &OnboardProfilesInfo,
) -> impl Iterator<Item = (usize, ProfileEntry)> + '_ {
    info.directory
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, entry)| entry.enabled && !entry.is_rom())
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
        .on_click(move |_, _, cx| AppState::update_onboard_profiles(cx, target, profile))
}

fn profile_button(index: usize, entry: ProfileEntry, selected: bool) -> Button {
    let sector = entry.sector;
    Button::new(SharedString::from(format!("profile-button-{sector}")))
        .compact()
        .label(tr!("Profile %{n}", n => (index + 1).to_string()))
        .selected(selected)
        .on_click(move |_, _, cx| {
            AppState::update_onboard_profiles(cx, ProfilesMode::Onboard, Some(sector));
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
        assert_eq!(
            selectable_profiles(&info).collect::<Vec<_>>(),
            vec![(2, info.directory[2])]
        );
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
