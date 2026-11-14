//! The Live tab of a keypad grid key: make the key show a value — the clock,
//! the volume, a battery, a command's output — refreshed on its own, and say
//! what a press and the two keys under the grid run while it is active.
//!
//! The commands are typed into fields and saved when a field loses focus or
//! Enter is pressed, as the label is: every save repaints the key.

use gpui::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement,
    SharedString, Styled, Subscription, Window, div, px,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{Selectable as _, h_flex, v_flex};
use openlogi_core::binding::{Action, GridSlot};
use openlogi_core::config::{LiveKey, LiveSource};

use super::FunctionRowView;
use crate::features::binding_editor::editor_section;
use crate::state::AppState;
use crate::ui::components::{Pill, Toggle, control_input};
use crate::ui::theme::{Palette, Typography as _};

/// Refresh intervals offered, in seconds.
const REFRESH: [u16; 4] = [1, 5, 30, 60];

/// The text fields of the live key being edited.
pub(super) struct LiveDraft {
    slot: GridSlot,
    /// The device the key belongs to, and the live key the fields were
    /// filled from: a draft for another device, or for a key changed under
    /// it (a restored config version), is made afresh rather than shown, and
    /// committed, over it.
    device: Option<String>,
    seeded: Option<LiveKey>,
    /// The source command, then the press, decrement and increment commands.
    fields: [Entity<InputState>; 4],
    _subscriptions: Vec<Subscription>,
}

/// Which field a [`LiveDraft`] input is.
#[derive(Clone, Copy)]
enum Field {
    Source,
    Press,
    Decrement,
    Increment,
}

impl Field {
    const ALL: [Self; 4] = [Self::Source, Self::Press, Self::Decrement, Self::Increment];
}

/// The command text an action is edited as: a shell command's text, else
/// nothing (other actions are kept as they are until a command replaces them).
fn command_text(action: Option<&Action>) -> String {
    match action {
        Some(Action::RunShellCommand(command)) => command.clone(),
        _ => String::new(),
    }
}

/// The action a typed command makes: `None` when it is empty.
pub(super) fn command_action(text: &str) -> Option<Action> {
    let text = text.trim();
    (!text.is_empty()).then(|| Action::RunShellCommand(text.to_owned()))
}

impl FunctionRowView {
    pub(super) fn ensure_live_draft(
        &mut self,
        slot: GridSlot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = AppState::try_read(cx);
        let device = state
            .and_then(AppState::current_record)
            .map(|record| record.device_key().to_string());
        let live = state.and_then(|state| state.current_live_key(slot));
        if self.live.as_ref().is_some_and(|draft| {
            draft.slot == slot && draft.device == device && draft.seeded == live
        }) {
            return;
        }
        let seeds = [
            match live.as_ref().map(|live| &live.source) {
                Some(LiveSource::Command(command)) => command.clone(),
                _ => String::new(),
            },
            command_text(live.as_ref().and_then(|live| live.on_press.as_ref())),
            command_text(live.as_ref().and_then(|live| live.decrement.as_ref())),
            command_text(live.as_ref().and_then(|live| live.increment.as_ref())),
        ];
        let placeholders = [
            tr!("keyboard.live_command_placeholder"),
            tr!("keyboard.live_press_placeholder"),
            tr!("keyboard.live_decrement_placeholder"),
            tr!("keyboard.live_increment_placeholder"),
        ];
        let mut subscriptions = Vec::new();
        let fields = Field::ALL.map(|field| {
            let index = field as usize;
            let seed = seeds[index].clone();
            let placeholder = placeholders[index].clone();
            let input = cx.new(|cx| {
                let mut input = InputState::new(window, cx).placeholder(placeholder);
                if !seed.is_empty() {
                    input.set_value(seed, window, cx);
                }
                input
            });
            subscriptions.push(
                cx.subscribe(&input, move |view, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        view.commit_live_field(field, cx);
                    }
                }),
            );
            input
        });
        self.live = Some(LiveDraft {
            slot,
            device,
            seeded: live,
            fields,
            _subscriptions: subscriptions,
        });
    }

    fn commit_live_field(&mut self, field: Field, cx: &mut Context<Self>) {
        let Some(draft) = &self.live else { return };
        let slot = draft.slot;
        let text = draft.fields[field as usize].read(cx).value().to_string();
        let Some(mut live) = AppState::try_read(cx).and_then(|state| state.current_live_key(slot))
        else {
            return;
        };
        let current = match field {
            Field::Source => match &live.source {
                LiveSource::Command(command) => command.clone(),
                _ => String::new(),
            },
            Field::Press => command_text(live.on_press.as_ref()),
            Field::Decrement => command_text(live.decrement.as_ref()),
            Field::Increment => command_text(live.increment.as_ref()),
        };
        // An untouched field changes nothing: leaving one that shows no text
        // for an action other than a shell command keeps that action.
        if text.trim() == current.trim() {
            return;
        }
        match field {
            Field::Source => {
                if !matches!(live.source, LiveSource::Command(_)) || text.trim().is_empty() {
                    return;
                }
                live.source = LiveSource::Command(text.trim().to_owned());
            }
            Field::Press => live.on_press = command_action(&text),
            Field::Decrement => live.decrement = command_action(&text),
            Field::Increment => live.increment = command_action(&text),
        }
        if let Some(draft) = &mut self.live {
            // The fields already show what is committed: no new draft for it.
            draft.seeded = Some(live.clone());
        }
        AppState::apply(cx, |state| state.commit_live_key(slot, Some(live)));
    }

    pub(super) fn live_panel(&self, slot: GridSlot, pal: Palette, cx: &App) -> impl IntoElement {
        let state = AppState::try_read(cx);
        let live = state.and_then(|state| state.current_live_key(slot));
        let batteries: Vec<(String, String)> = state
            .map(AppState::live_battery_devices)
            .unwrap_or_default();
        let enabled = live.is_some();
        let toggle = Toggle::new("live-key-enabled")
            .accessibility_label(tr!("keyboard.live_show_value"))
            .selected(enabled)
            .on_change(move |on, _, cx| {
                let live = on.then(|| LiveKey::new(LiveSource::Clock));
                AppState::apply(cx, |state| state.commit_live_key(slot, live));
            });
        let mut content = v_flex()
            .id("live-key")
            .debug_selector(|| "live-key".into())
            .gap_2()
            .px_2()
            .pb_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(div().text_body().child(tr!("keyboard.live_show_value")))
                    .child(toggle),
            )
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("keyboard.live_description")),
            );
        let Some(live) = live else {
            return content;
        };
        let draft = self.live.as_ref().filter(|draft| draft.slot == slot);
        content = content
            .child(editor_section(tr!("keyboard.live_source"), pal))
            .child(source_pills(slot, &live, &batteries));
        if matches!(live.source, LiveSource::Command(_))
            && let Some(draft) = draft
        {
            content = content.child(control_input(&draft.fields[Field::Source as usize]));
        }
        content = content
            .child(editor_section(tr!("keyboard.live_refresh"), pal))
            .child(h_flex().gap_1().children(REFRESH.iter().map(|&secs| {
                let mut next = live.clone();
                next.refresh_secs = secs;
                Pill::new(
                    SharedString::from(format!("live-refresh-{secs}")),
                    tr!("keyboard.live_seconds", secs = secs),
                )
                .selected(live.refresh_secs == secs)
                .on_click(move |_, _, cx| {
                    let next = next.clone();
                    AppState::apply(cx, |state| state.commit_live_key(slot, Some(next)));
                })
            })))
            .child(editor_section(tr!("keyboard.live_actions"), pal))
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("keyboard.live_actions_description")),
            );
        if let Some(draft) = draft {
            content = content
                .child(control_input(&draft.fields[Field::Press as usize]))
                .child(control_input(&draft.fields[Field::Decrement as usize]))
                .child(
                    div()
                        .debug_selector(|| "live-key-increment".into())
                        .child(control_input(&draft.fields[Field::Increment as usize])),
                );
        }
        content.child(div().h(px(2.)))
    }
}

/// One pill per source; a battery source offers each device.
fn source_pills(slot: GridSlot, live: &LiveKey, batteries: &[(String, String)]) -> gpui::Div {
    let choice = |id: String, label: SharedString, source: LiveSource, selected: bool| {
        let mut next = live.clone();
        next.source = source;
        Pill::new(SharedString::from(id), label)
            .selected(selected)
            .on_click(move |_, _, cx| {
                let next = next.clone();
                AppState::apply(cx, |state| state.commit_live_key(slot, Some(next)));
            })
    };
    let mut pills = vec![
        choice(
            "live-source-clock".into(),
            tr!("keyboard.live_source_clock"),
            LiveSource::Clock,
            live.source == LiveSource::Clock,
        ),
        choice(
            "live-source-volume".into(),
            tr!("keyboard.live_source_volume"),
            LiveSource::Volume,
            live.source == LiveSource::Volume,
        ),
        choice(
            "live-source-command".into(),
            tr!("keyboard.live_source_command"),
            match &live.source {
                LiveSource::Command(command) => LiveSource::Command(command.clone()),
                _ => LiveSource::Command(LiveSource::example_command().to_owned()),
            },
            matches!(live.source, LiveSource::Command(_)),
        ),
    ];
    for (index, (key, name)) in batteries.iter().enumerate() {
        pills.push(choice(
            format!("live-source-battery-{index}"),
            tr!("keyboard.live_source_battery", device = name),
            LiveSource::Battery(key.clone()),
            live.source == LiveSource::Battery(key.clone()),
        ));
    }
    h_flex().gap_1().flex_wrap().children(pills)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_command_is_a_shell_action_and_an_empty_one_is_none() {
        assert_eq!(command_action("  "), None);
        assert_eq!(
            command_action(" pactl set-sink-volume @DEFAULT_SINK@ +5% "),
            Some(Action::RunShellCommand(
                "pactl set-sink-volume @DEFAULT_SINK@ +5%".into()
            ))
        );
        assert_eq!(command_text(Some(&Action::Copy)), "");
    }
}
