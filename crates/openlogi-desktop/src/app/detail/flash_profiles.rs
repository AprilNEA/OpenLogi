//! The Pointer tab's profiles card for a mouse whose flash is the source of
//! truth (the G700): the five directory slots as the mouse lists them, which
//! one it plays, and the edits OpenLogi makes there — switch, rename, copy,
//! disable, enable again.
//!
//! Nothing is stored on this computer. Every edit but a switch rewrites
//! flash, which the agent queues and runs to its end; while it does, the card
//! says so and offers no further edit, and it shows the mouse's reading once
//! the write lands.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, Window, div, px,
};
use gpui_kit::component::{
    Disableable as _, Icon, Selectable as _, WindowExt as _,
    button::{Button, ButtonGroup},
    h_flex,
    input::InputState,
    v_flex,
};
use openlogi_core::binding::FlashWrite;
use openlogi_core::hid::{
    FLASH_PROFILE_NAME_UNITS as NAME_UNITS, FlashProfileEdit, FlashProfiles, OnboardProfile,
    ProfileIndex,
};

use crate::state::{AppState, DeviceKey, FlashProfilesLoad};
use crate::ui::components::{PanelCard, control_input, save_cancel_footer};
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, Typography as _};

/// What the card needs about the selected mouse.
pub(super) struct FlashProfilesFacts {
    key: DeviceKey,
    load: FlashProfilesLoad,
    reachable: bool,
    refusal: Option<String>,
}

impl FlashProfilesFacts {
    /// The selected mouse's profile facts, or `None` when it keeps no flash
    /// profiles OpenLogi edits.
    pub(super) fn current(cx: &App) -> Option<Self> {
        let state = AppState::try_read(cx)?;
        if !state.current_flash_profiles_supported() {
            return None;
        }
        let record = state.current_record()?;
        Some(Self {
            key: record.device_key(),
            load: state.current_flash_profiles_load(),
            reachable: record.route.is_some(),
            refusal: state.current_flash_edit_refusal().map(str::to_owned),
        })
    }
}

/// The card, or `None` for a mouse without flash profiles, or an offline one
/// with no reading to show.
pub(super) fn flash_profiles_card(
    facts: Option<FlashProfilesFacts>,
    pal: Palette,
) -> Option<AnyElement> {
    let FlashProfilesFacts {
        key,
        load,
        reachable,
        refusal,
    } = facts?;
    let body: AnyElement = match load {
        FlashProfilesLoad::Ready(profiles) => {
            profiles_body(&profiles, reachable, refusal.as_deref(), pal).into_any_element()
        }
        FlashProfilesLoad::Unknown | FlashProfilesLoad::Loading if !reachable => return None,
        FlashProfilesLoad::Unknown | FlashProfilesLoad::Loading => {
            status_line(tr!("pointer.reading_flash_profiles"), pal).into_any_element()
        }
        FlashProfilesLoad::Failed(_) => retry_line(
            "flash-profiles-retry",
            tr!("pointer.couldnt_read_flash_profiles_click_to_retry"),
            move |cx| AppState::apply(cx, |state| state.retry_flash_profiles_read(&key)),
        )
        .into_any_element(),
        FlashProfilesLoad::Unsupported(_) => return None,
    };
    Some(
        PanelCard::new(
            tr!("pointer.onboard_memory"),
            Icon::empty().path("action-icons/mouse.svg"),
            body,
        )
        .fill()
        .into_any_element(),
    )
}

/// The mouse is busy with a flash write, or cannot be reached: no edit is
/// offered until it is free again.
fn editable(profiles: &FlashProfiles, reachable: bool) -> bool {
    reachable && !matches!(profiles.write, FlashWrite::Pending | FlashWrite::Writing)
}

fn profiles_body(
    profiles: &Arc<FlashProfiles>,
    reachable: bool,
    refusal: Option<&str>,
    pal: Palette,
) -> impl IntoElement {
    let editable = editable(profiles, reachable);
    let enabled: Vec<&OnboardProfile> = profiles
        .onboard
        .profiles
        .iter()
        .filter(|p| p.enabled)
        .collect();
    v_flex()
        .gap_4()
        .child(
            v_flex()
                .gap_2()
                .child(
                    v_flex()
                        .child(
                            div()
                                .text_body()
                                .text_color(pal.text_primary)
                                .child(tr!("pointer.flash_active_profile")),
                        )
                        .child(
                            div()
                                .text_caption()
                                .text_color(pal.text_muted)
                                .child(tr!("pointer.flash_profiles_description")),
                        ),
                )
                .child(active_control(
                    profiles.onboard.active_profile,
                    &enabled,
                    editable,
                )),
        )
        .child(
            v_flex()
                .gap_3()
                .children(
                    enabled
                        .iter()
                        .map(|profile| listed_row(profiles, profile, editable, pal)),
                )
                .children(profiles.disabled.iter().enumerate().map(|(row, disabled)| {
                    unlisted_row(
                        row,
                        disabled.page,
                        disabled.name.as_deref(),
                        editable && profiles.has_free_slot(),
                        pal,
                    )
                })),
        )
        .children(write_status(&profiles.write, pal))
        .children(refusal.map(|refusal| {
            div()
                .debug_selector(|| "flash-profiles-refusal".into())
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("pointer.flash_profile_edit_refused", error => refusal))
        }))
}

/// One button per listed profile; the one the mouse plays is selected.
fn active_control(
    active: Option<ProfileIndex>,
    enabled: &[&OnboardProfile],
    editable: bool,
) -> impl IntoElement {
    let values: Vec<ProfileIndex> = enabled.iter().map(|p| p.index).collect();
    enabled
        .iter()
        .enumerate()
        .fold(
            // The group hands its own disabled state to each child as it is
            // added, so it is set before the children.
            ButtonGroup::new("flash-active-profile")
                .w_full()
                .outline()
                .disabled(!editable),
            |group, (index, profile)| {
                group.child(
                    Button::new(("flash-active-profile", index))
                        .flex_1()
                        .label(profile_name(profile.index, profile.name.as_deref()).text)
                        .selected(active == Some(profile.index)),
                )
            },
        )
        .on_click(move |indices, _window, cx| {
            if let Some(profile) = indices.first().and_then(|i| values.get(*i))
                && active != Some(*profile)
            {
                AppState::edit_flash_profiles(cx, FlashProfileEdit::Activate(*profile));
            }
        })
}

/// A listed profile: its name over its slot, then a line of rename, copy
/// and disable.
fn listed_row(
    profiles: &FlashProfiles,
    profile: &OnboardProfile,
    editable: bool,
    pal: Palette,
) -> impl IntoElement {
    let index = profile.index;
    let number = usize::from(index.get().get());
    let name = profile_name(index, profile.name.as_deref());
    let mut details = vec![tr!("pointer.flash_profile_slot", number => number).to_string()];
    if profiles.onboard.active_profile == Some(index) {
        details.push(tr!("pointer.flash_profile_playing").to_string());
    }
    if name.damaged {
        details.push(tr!("pointer.flash_profile_name_damaged").to_string());
    }
    let can_copy = editable && profiles.can_create && profiles.has_free_slot();
    let can_disable = editable && profiles.can_disable(index);
    let current_name = profile.name.clone().filter(|_| !name.damaged);
    // The edits sit on their own wrapping line under the name: three
    // buttons beside a name would squeeze it to nothing in a narrow card or
    // a long translation.
    let edits = h_flex()
        .flex_wrap()
        .gap_2()
        .child(
            Button::new(("flash-profile-rename", number))
                .debug_selector(move || format!("flash-profile-{number}-rename"))
                .compact()
                .label(tr!("common.rename_dialog"))
                .disabled(!editable)
                .on_click(move |_event, window, cx| {
                    open_rename_dialog(window, cx, index, current_name.as_deref());
                }),
        )
        .child(
            Button::new(("flash-profile-copy", number))
                .debug_selector(move || format!("flash-profile-{number}-copy"))
                .compact()
                .label(tr!("pointer.flash_profile_copy"))
                .disabled(!can_copy)
                .on_click(move |_event, _window, cx| {
                    AppState::edit_flash_profiles(cx, FlashProfileEdit::Duplicate(index));
                }),
        )
        .child(
            Button::new(("flash-profile-disable", number))
                .debug_selector(move || format!("flash-profile-{number}-disable"))
                .compact()
                .label(tr!("pointer.flash_profile_disable"))
                .disabled(!can_disable)
                .on_click(move |_event, _window, cx| {
                    AppState::edit_flash_profiles(cx, FlashProfileEdit::Disable(index));
                }),
        );
    v_flex()
        .id(("flash-profile", number))
        .debug_selector(move || format!("flash-profile-{number}"))
        .w_full()
        .min_w_0()
        .gap_1p5()
        .child(name_column(name.text, details.join(" · "), true, pal))
        .child(edits)
}

/// A profile the mouse keeps but does not list, over an enable that works
/// while a slot is free.
fn unlisted_row(
    row: usize,
    page: u8,
    name: Option<&str>,
    can_enable: bool,
    pal: Palette,
) -> impl IntoElement {
    let shown = match name.map(display_name) {
        Some(DisplayName {
            text,
            damaged: false,
        }) if !text.is_empty() => text,
        _ => tr!("pointer.flash_profile_unnamed"),
    };
    v_flex()
        .id(("flash-unlisted", row))
        .debug_selector(move || format!("flash-unlisted-{row}"))
        .w_full()
        .min_w_0()
        .gap_1p5()
        .child(name_column(
            shown,
            tr!("pointer.flash_profile_not_listed").to_string(),
            false,
            pal,
        ))
        .child(
            h_flex().child(
                Button::new(("flash-unlisted-enable", row))
                    .debug_selector(move || format!("flash-unlisted-{row}-enable"))
                    .compact()
                    .label(tr!("pointer.flash_profile_enable"))
                    .disabled(!can_enable)
                    .on_click(move |_event, _window, cx| {
                        AppState::edit_flash_profiles(cx, FlashProfileEdit::Enable { page });
                    }),
            ),
        )
}

fn name_column(
    title: SharedString,
    details: String,
    listed: bool,
    pal: Palette,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_w_0()
        .gap_0p5()
        .child(
            div()
                .text_body()
                .text_color(if listed {
                    pal.text_primary
                } else {
                    pal.text_muted
                })
                .child(title),
        )
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(details),
        )
}

/// Where the queued flash write stands, when it is not idle.
fn write_status(write: &FlashWrite, pal: Palette) -> Option<impl IntoElement> {
    let text = match write {
        FlashWrite::Idle => return None,
        FlashWrite::Pending | FlashWrite::Writing => tr!("actions.flash_writing"),
        FlashWrite::Failed(error) => tr!("actions.flash_write_failed", error => error),
    };
    Some(
        div()
            .debug_selector(|| "flash-profiles-write".into())
            .text_caption()
            .text_color(pal.text_muted)
            .child(text),
    )
}

/// A stored name as the card shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DisplayName {
    text: SharedString,
    /// The stored name holds control characters, which no name typed into
    /// Logitech Gaming Software or OpenLogi has: real G700s keep names whose
    /// UTF-16 high bytes were lost, which read as control characters.
    damaged: bool,
}

/// `name` with control characters shown as U+FFFD, trimmed.
fn display_name(name: &str) -> DisplayName {
    let damaged = name.chars().any(char::is_control);
    let text: String = name
        .chars()
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .collect();
    DisplayName {
        text: text.trim().to_owned().into(),
        damaged,
    }
}

/// A listed profile's name: the stored one, or "Profile N" for none or a
/// blank one.
fn profile_name(index: ProfileIndex, name: Option<&str>) -> DisplayName {
    match name.map(display_name) {
        Some(shown) if !shown.text.is_empty() => shown,
        _ => DisplayName {
            text: tr!("pointer.onboard_profile_number", number => index.to_string()),
            damaged: false,
        },
    }
}

/// Whether a typed name fits a profile: trimmed, not empty, at most
/// [`NAME_UNITS`] UTF-16 units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameCheck {
    Fits { used: usize },
    Empty,
    TooLong { used: usize },
}

fn name_check(value: &str) -> NameCheck {
    let name = value.trim();
    match name.encode_utf16().count() {
        0 => NameCheck::Empty,
        used if used > NAME_UNITS => NameCheck::TooLong { used },
        used => NameCheck::Fits { used },
    }
}

fn name_caption(check: NameCheck, tried: bool) -> SharedString {
    let max = NAME_UNITS;
    match check {
        NameCheck::Fits { used } => {
            tr!("pointer.flash_profile_name_length", used => used, max => max)
        }
        NameCheck::TooLong { used } => {
            tr!("pointer.flash_profile_name_too_long", used => used, max => max)
        }
        NameCheck::Empty if tried => tr!("pointer.flash_profile_name_empty"),
        NameCheck::Empty => tr!("pointer.flash_profile_name_length", used => 0, max => max),
    }
}

fn open_rename_dialog(
    window: &mut Window,
    cx: &mut App,
    profile: ProfileIndex,
    name: Option<&str>,
) {
    let input = cx.new(|cx| {
        let mut input = InputState::new(window, cx).placeholder(tr!("pointer.flash_profile_name"));
        if let Some(name) = name {
            input.set_value(name.to_owned(), window, cx);
        }
        input
    });
    // Set once Save is pressed on a name that does not fit, so an empty field
    // says why Save did nothing.
    let tried = Rc::new(Cell::new(false));
    window.open_dialog(cx, move |dialog, window, cx| {
        input.update(cx, |input, cx| input.focus(window, cx));
        let pal = theme::palette(cx);
        let caption = name_caption(name_check(&typed(&input, cx)), tried.get());
        dialog
            .w(px(420.))
            .title(tr!("pointer.rename_flash_profile", number => profile.to_string()))
            .child(
                v_flex().gap_2().child(control_input(&input)).child(
                    div()
                        .debug_selector(|| "flash-profile-name-caption".into())
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(caption),
                ),
            )
            .footer(save_cancel_footer())
            .on_ok({
                let input = input.clone();
                let tried = tried.clone();
                move |_, window, cx| {
                    let name = typed(&input, cx);
                    if !matches!(name_check(&name), NameCheck::Fits { .. }) {
                        tried.set(true);
                        window.refresh();
                        return false;
                    }
                    AppState::edit_flash_profiles(
                        cx,
                        FlashProfileEdit::Rename {
                            profile,
                            name: name.trim().to_owned(),
                        },
                    );
                    true
                }
            })
    });
}

/// The field's text without NULs, which a profile name cannot hold.
fn typed(input: &gpui::Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().replace('\0', "")
}

#[cfg(test)]
mod tests;
