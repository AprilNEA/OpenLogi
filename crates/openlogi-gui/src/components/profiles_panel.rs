//! Onboard-profiles controls for gaming mice (HID++ `0x8100`).
//!
//! Two source pills — OpenLogi settings (host mode) vs. onboard memory — and,
//! under onboard memory, a selector for which stored profile is active. Each
//! change is written to the device *and* persisted to `config.toml` (via
//! [`AppState::commit_onboard_profiles`]): the mode lives in device RAM and
//! reverts to onboard on a power cycle, so the agent re-applies the saved
//! config when the device reconnects. State is read lazily on the same
//! background pattern as [`crate::components::smartshift_panel`].

use gpui::{
    AnyElement, BorrowAppContext as _, Context, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement as _, Styled, Subscription, Window, div,
};
use gpui_component::{h_flex, v_flex};
use openlogi_hid::{DeviceRoute, OnboardProfilesInfo, ProfileEntry, ProfilesMode};

use crate::components::device_read::issue_device_read;
use crate::components::status::{retry_line, status_line};
use crate::state::{AppState, ProfilesLoad};
use crate::theme::{self, Palette, SelectableStyle, Typography as _};

pub struct ProfilesPanel {
    _state_obs: Subscription,
}

impl ProfilesPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = cx.observe_global::<AppState>(|_, cx| cx.notify());
        Self {
            _state_obs: state_obs,
        }
    }

    /// Kick off a one-shot onboard-profiles read for the active device when it
    /// hasn't been queried yet.
    fn ensure_profiles_load(cx: &mut Context<Self>) {
        let Some((key, route)) = profiles_load_target(cx) else {
            return;
        };
        cx.update_global::<AppState, _>(|state, _| state.mark_profiles_loading(&key));
        Self::issue_profiles_read(key, route, cx);
    }

    /// Re-read once after an optimistic write to confirm the device actually
    /// took it. No Loading marker, so the optimistic value stays on screen
    /// until the real state replaces it.
    fn ensure_profiles_confirm(cx: &mut Context<Self>) {
        let Some((key, route)) =
            cx.update_global::<AppState, _>(|state, _| state.take_active_profiles_confirm())
        else {
            return;
        };
        Self::issue_profiles_read(key, route, cx);
    }

    fn issue_profiles_read(key: String, route: DeviceRoute, cx: &mut Context<Self>) {
        issue_device_read(
            cx,
            key,
            route,
            crate::ipc_client::Command::ReadOnboardProfiles,
            AppState::store_profiles_info,
            AppState::clear_profiles_loading,
        );
    }

    /// The interactive body shown once the device's profiles state resolves.
    fn ready_body(info: &OnboardProfilesInfo, pal: Palette) -> AnyElement {
        let onboard = info.mode == ProfilesMode::Onboard;
        // In host mode there is nothing to select; keep whatever profile the
        // device has active so switching back to onboard restores it.
        let keep_profile = keep_profile_for(info);

        let source_row = v_flex()
            .gap_2()
            .child(section_label(tr!("Settings source"), pal))
            .child(
                h_flex()
                    .gap_2()
                    .child(source_pill(
                        "profiles-source-host",
                        tr!("OpenLogi settings"),
                        !onboard,
                        ProfilesMode::Host,
                        keep_profile,
                        pal,
                    ))
                    .child(source_pill(
                        "profiles-source-onboard",
                        tr!("Onboard memory"),
                        onboard,
                        ProfilesMode::Onboard,
                        keep_profile,
                        pal,
                    )),
            )
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(if onboard {
                        tr!("The mouse runs the profile stored in its memory; OpenLogi settings do not apply.")
                    } else {
                        tr!("OpenLogi drives this mouse; the onboard profile is dormant.")
                    }),
            );

        let mut body = v_flex().gap_4().w_full().child(source_row);
        if onboard {
            let enabled: Vec<&ProfileEntry> = info.directory.iter().filter(|e| e.enabled).collect();
            let profile_row = v_flex()
                .gap_2()
                .child(section_label(tr!("Active onboard profile"), pal))
                .child(if enabled.is_empty() {
                    status_line(tr!("No enabled profiles in the device's memory."), pal)
                } else {
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .children(enabled.iter().enumerate().map(|(i, entry)| {
                            profile_pill(i, **entry, entry.sector == info.active_profile, pal)
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

        let status = cx
            .try_global::<AppState>()
            .map_or(ProfilesLoad::Unknown, AppState::current_profiles_status);
        let reachable = cx
            .try_global::<AppState>()
            .and_then(AppState::current_record)
            .is_some_and(|r| r.route.is_some());

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
                |cx| {
                    cx.update_global::<AppState, _>(|state, _| state.retry_active_profiles());
                    cx.refresh_windows();
                },
            ),
            ProfilesLoad::Unsupported(_) => {
                status_line(tr!("This device has no onboard profile memory."), pal)
            }
        };

        v_flex().gap_3().w_full().child(content)
    }
}

fn profiles_load_target(cx: &mut Context<ProfilesPanel>) -> Option<(String, DeviceRoute)> {
    cx.try_global::<AppState>().and_then(|state| {
        if !state.current_profiles_unqueried() {
            return None;
        }
        let record = state.current_record()?;
        Some((record.config_key.clone(), record.route.clone()?))
    })
}

/// The profile sector a mode switch should carry: the currently active one
/// when it is a real, enabled entry, otherwise the first enabled entry (a
/// device that has never run onboard reports `0x0000`).
fn keep_profile_for(info: &OnboardProfilesInfo) -> Option<u16> {
    let enabled = |sector: u16| {
        info.directory
            .iter()
            .any(|e| e.enabled && e.sector == sector)
    };
    if info.active_profile != 0 && enabled(info.active_profile) {
        Some(info.active_profile)
    } else {
        info.directory.iter().find(|e| e.enabled).map(|e| e.sector)
    }
}

/// A small muted section heading.
fn section_label(text: SharedString, pal: Palette) -> AnyElement {
    div()
        .text_body()
        .text_color(pal.text_muted)
        .child(text)
        .into_any_element()
}

/// One settings-source pill. Clicking it commits `target`, carrying the
/// profile selection along so an onboard switch activates a real profile.
fn source_pill(
    id: &'static str,
    label: SharedString,
    selected: bool,
    target: ProfilesMode,
    profile: Option<u16>,
    pal: Palette,
) -> AnyElement {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded(pal.control_radius)
        .selected_border(selected, pal)
        .bg(pal.surface)
        .selected_fill(selected)
        .text_body()
        .text_color(pal.text_primary)
        .cursor_pointer()
        .hover(|s| s.bg(pal.surface_hover))
        .child(label)
        .on_click(move |_event, _window, cx| {
            cx.update_global::<AppState, _>(|state, _| {
                state.commit_onboard_profiles(target, profile);
            });
            cx.refresh_windows();
        })
        .into_any_element()
}

/// One enabled directory entry as a selectable pill, labeled by its position
/// (ROM profiles are labeled as such).
fn profile_pill(index: usize, entry: ProfileEntry, selected: bool, pal: Palette) -> AnyElement {
    let n = (index + 1).to_string();
    let label = if entry.is_rom() {
        tr!("ROM profile %{n}", n => n)
    } else {
        tr!("Profile %{n}", n => n)
    };
    let sector = entry.sector;
    div()
        .id(SharedString::from(format!("profile-pill-{sector}")))
        .px_3()
        .py_1()
        .rounded(pal.control_radius)
        .selected_border(selected, pal)
        .bg(pal.surface)
        .selected_fill(selected)
        .text_body()
        .text_color(pal.text_primary)
        .cursor_pointer()
        .hover(|s| s.bg(pal.surface_hover))
        .child(label)
        .on_click(move |_event, _window, cx| {
            cx.update_global::<AppState, _>(|state, _| {
                state.commit_onboard_profiles(ProfilesMode::Onboard, Some(sector));
            });
            cx.refresh_windows();
        })
        .into_any_element()
}
