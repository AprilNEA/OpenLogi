//! Performance / endurance power-mode toggle for the pointer-detail column.
//!
//! Drives HID++ `0x8090 ModeStatus` (G305 and friends): endurance holds the
//! slowest report rate for months of battery life, performance unlocks the
//! full report-rate range. The device persists the mode itself, so there is
//! no config commit — reads come from the swr-backed device query, writes go
//! straight to the agent with a confirming re-read. The toggle greys out when
//! `getDevConfig` reports no software switch.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, IntoElement, ParentElement, Render, Styled, Subscription, Window, div,
};
use gpui_component::{Disableable as _, Selectable as _, h_flex, v_flex};
use openlogi_core::hid::{PowerMode, PowerModeState};

use crate::state::{AppState, DeviceKey, PowerModeLoad, StateEvent};
use crate::ui::components::Toggle;
use crate::ui::section::section_label;
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, Typography as _};

pub struct PowerModePanel {
    _state_obs: Subscription,
}

impl PowerModePanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = cx.subscribe(&AppState::global(cx), |_, _, event: &StateEvent, cx| {
            let relevant = match event {
                StateEvent::InventoryChanged | StateEvent::DeviceSelected(_) => true,
                StateEvent::PowerModeChanged(key) => AppState::try_read(cx)
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
}

impl Render for PowerModePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);

        let (key, status) = AppState::try_read(cx)
            .and_then(|state| {
                let key = state.current_record()?.device_key();
                Some((Some(key.clone()), state.power_mode_status_for(&key)))
            })
            .unwrap_or((None, PowerModeLoad::Unknown));
        let reachable = AppState::try_read(cx)
            .and_then(AppState::current_record)
            .is_some_and(|r| r.route.is_some());

        let content: AnyElement = match status {
            PowerModeLoad::Ready(state) => ready_body(*state, pal).into_any_element(),
            PowerModeLoad::Loading | PowerModeLoad::Unknown if !reachable => {
                status_line(tr!("pointer.device_offline_power_mode_is_unavailable"), pal)
                    .into_any_element()
            }
            PowerModeLoad::Loading | PowerModeLoad::Unknown => {
                status_line(tr!("pointer.reading_power_mode"), pal).into_any_element()
            }
            PowerModeLoad::Failed(_) => retry_line(
                "power-mode-retry",
                tr!("pointer.couldnt_read_power_mode_click_to_retry"),
                pal,
                retry_power_mode_closure(key),
            )
            .into_any_element(),
            PowerModeLoad::Unsupported(_) => {
                status_line(tr!("pointer.this_device_does_not_support_power_mode"), pal)
                    .into_any_element()
            }
        };

        v_flex().gap_3().w_full().child(content)
    }
}

/// The interactive body shown once the device's power mode resolves.
fn ready_body(state: PowerModeState, pal: Palette) -> gpui::Div {
    let performance = matches!(state.mode, PowerMode::Performance);
    v_flex()
        .gap_2()
        .w_full()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .gap_3()
                .child(section_label(tr!("pointer.performance_mode"), pal))
                .child(
                    Toggle::new("power-mode-performance")
                        .selected(performance)
                        .disabled(!state.software_switch)
                        .on_change(|performance, _window, cx| {
                            let mode = if *performance {
                                PowerMode::Performance
                            } else {
                                PowerMode::Endurance
                            };
                            AppState::update_power_mode(cx, mode);
                        }),
                ),
        )
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("pointer.power_mode_description")),
        )
        // The faster rate shrinks per-report deltas 8x, and macOS's pointer
        // acceleration gives small deltas less gain — the cursor feels slower
        // at an unchanged DPI. Verified on a G305: DPI reads identically in
        // both modes. Say so, or every macOS tester files a DPI bug.
        .when(cfg!(target_os = "macos"), |body| {
            body.child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("pointer.power_mode_macos_pointer_note")),
            )
        })
        .when(!state.software_switch, |body| {
            body.child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("pointer.power_mode_hardware_switch_only")),
            )
        })
}

/// A retry action bound to `key`, or a no-op when there is no active device.
fn retry_power_mode_closure(key: Option<DeviceKey>) -> impl Fn(&mut App) + 'static {
    move |cx| {
        if let Some(key) = &key {
            AppState::retry_power_mode_read(cx, key.clone());
        }
    }
}
