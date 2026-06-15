//! Diagnostics settings page (macOS): flags other apps intercepting the mouse
//! event stream — a common pointer-lag cause — and, in debug builds, dumps the
//! full event-tap list plus a live event monitor.

use openlogi_hook::Hook;

#[cfg(debug_assertions)]
use super::AppState;
use super::{
    AnyElement, App, Axis, IconName, IntoElement, Palette, ParentElement, SettingField,
    SettingGroup, SettingItem, SettingPage, Styled, div, rgb, theme, v_flex,
};

/// The Diagnostics page: the curated input-conflict check, plus (debug) the raw
/// tap list and the live event monitor polled by
/// [`SettingsView`](super::SettingsView)'s task.
pub(super) fn diagnostics_page(pal: Palette) -> SettingPage {
    SettingPage::new(tr!("Diagnostics"))
        .icon(IconName::Info)
        .resettable(false)
        .group(
            SettingGroup::new().item(
                SettingItem::new(
                    tr!("Input interception"),
                    SettingField::render(move |_, _, cx| input_conflict_field(pal, cx)),
                )
                .description(tr!(
                    "Detects other apps tapping the mouse event stream — a common cause of pointer lag."
                ))
                // Vertical: the status + tap list are wide, multi-line content,
                // not a compact right-side control — stacking them full-width
                // below the title lets the lines wrap instead of overflowing.
                .layout(Axis::Vertical),
            ),
        )
}

/// Live status: the curated known-conflict check over the current event taps,
/// plus (debug) the full tap list. Recomputed on each render, so it reflects the
/// live tap set whenever the window repaints.
fn input_conflict_field(pal: Palette, cx: &mut App) -> AnyElement {
    let taps = Hook::list_event_taps();

    // Dedup the product names of input-gating taps owned by known conflicts.
    let mut conflicts: Vec<&'static str> = Vec::new();
    for tap in &taps {
        if tap.gates_input()
            && let Some(name) = tap.known_input_conflict()
            && !conflicts.contains(&name)
        {
            conflicts.push(name);
        }
    }

    let mut col = v_flex().w_full().gap_1();
    if conflicts.is_empty() {
        col = col.child(
            div()
                .text_xs()
                .text_color(rgb(theme::STATUS_CONNECTED))
                .child(tr!("No other app is intercepting mouse input.")),
        );
    } else {
        col = col.child(
            div()
                .text_sm()
                .text_color(rgb(theme::STATUS_CONNECTING))
                .child(tr!(
                    "Another app is intercepting mouse input, which can cause pointer lag or duplicated button actions: %{apps}",
                    apps => conflicts.join(", ")
                )),
        );
    }

    #[cfg(debug_assertions)]
    {
        col = col.child(debug_tap_list(&taps, pal));
        col = col.child(monitor_list(pal, cx));
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = (pal, cx);
    }

    col.into_any_element()
}

/// Debug-only live event monitor: the events the agent's hook has observed,
/// newest first. Polled into [`AppState`] by
/// [`SettingsView`](super::SettingsView)'s task.
#[cfg(debug_assertions)]
fn monitor_list(pal: Palette, cx: &mut App) -> impl IntoElement {
    let lines: Vec<String> = cx
        .try_global::<AppState>()
        .map(|s| {
            s.monitor_events()
                .iter()
                .rev()
                .take(20)
                .map(format_monitor_event)
                .collect()
        })
        .unwrap_or_default();

    let mut col = v_flex().w_full().mt_2().gap_1().child(
        div()
            .text_xs()
            .text_color(pal.text_muted)
            .child("Live events (newest first)"),
    );
    if lines.is_empty() {
        col = col.child(
            div()
                .text_xs()
                .text_color(pal.text_muted)
                .child("(click or scroll to see what the hook receives)"),
        );
    } else {
        for line in lines {
            col = col.child(div().text_xs().text_color(pal.text_primary).child(line));
        }
    }
    col
}

#[cfg(debug_assertions)]
fn format_monitor_event(event: &openlogi_agent_core::ipc::MonitorEvent) -> String {
    use openlogi_agent_core::ipc::MonitorEvent;
    match event {
        MonitorEvent::Button { button, pressed } => {
            format!("button {button} {}", if *pressed { "down" } else { "up" })
        }
        MonitorEvent::Scroll { delta_x, delta_y } => {
            format!("scroll dx={delta_x:.1} dy={delta_y:.1}")
        }
        MonitorEvent::CaptureInterrupted => "capture interrupted".to_string(),
    }
}

/// Debug-only raw dump of every event tap: owner, location, mode, enabled. Taps
/// that gate the HID stream are highlighted, since those are the lag-relevant
/// ones. English-only by design — a developer aid, not a shipped string.
#[cfg(debug_assertions)]
fn debug_tap_list(taps: &[openlogi_hook::EventTapInfo], pal: Palette) -> impl IntoElement {
    let mut col = v_flex().w_full().mt_2().gap_1().child(
        div()
            .text_xs()
            .text_color(pal.text_muted)
            .child(format!("{} event tap(s)", taps.len())),
    );
    for tap in taps {
        let owner = tap.owner_name.as_deref().unwrap_or("(unknown)");
        let mode = if tap.active { "active" } else { "listen" };
        let line = format!(
            "{owner} (pid {}) — {:?} {mode} enabled={}",
            tap.owner_pid, tap.location, tap.enabled
        );
        let row = div().text_xs().child(line);
        let row = if tap.gates_input() {
            row.text_color(rgb(theme::STATUS_CONNECTING))
        } else {
            row.text_color(pal.text_muted)
        };
        col = col.child(row);
    }
    col
}
