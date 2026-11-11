//! The appearance of a keypad's LCD key: the panel tab that edits the
//! symbol (see [`super::key_symbol`]), label, colours, picture, and icon the
//! agent paints on the key, and the small pure rules behind it.
//!
//! Each face belongs to one grid key on one binding page, like the key's
//! binding, and is saved through [`AppState::commit_key_face`].

use std::path::Path;

use gpui::{
    App, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    PathPromptOptions, SharedString, StatefulInteractiveElement as _, Styled, Subscription, Window,
    div, prelude::FluentBuilder as _, px,
};
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex};
use openlogi_core::binding::GridSlot;
use openlogi_core::config::{KeyFaceStyle, KeySymbol};

use super::FunctionRowView;
use super::key_colors::{ColorField, background_section, text_and_icon_colors};
use super::key_symbol::{SymbolMode, symbol_section};
use crate::features::binding_editor::editor_section;
use crate::state::AppState;
use crate::ui::components::{Toggle, control_input};
use crate::ui::theme::{Palette, SelectableStyle as _, Typography as _};

/// The tallest the Appearance tab grows before it scrolls, in px.
const APPEARANCE_MAX_H: f32 = 520.;

/// Which part of a grid key the config panel edits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum KeyPanelTab {
    /// The action the key runs.
    #[default]
    Action,
    /// How the key's LCD face looks.
    Appearance,
}

/// The inputs of the grid key being styled: the label's two lines, the icon
/// search, the emoji field, and the custom colours. The label, the emoji and
/// a colour are saved when their input loses focus or Enter is pressed in
/// it, not per keystroke: every save repaints the key.
pub(super) struct FaceDraft {
    pub(super) slot: GridSlot,
    lines: [Entity<InputState>; 2],
    /// The Lucide search query; the grid follows it as it is typed.
    pub(super) search: Entity<InputState>,
    pub(super) emoji: Entity<InputState>,
    /// Which kind of symbol the Symbol section is choosing.
    pub(super) symbol_mode: SymbolMode,
    /// Each colour chooser's custom hex input, in [`ColorField::ALL`] order.
    pub(super) hex: [Entity<InputState>; 5],
    _subscriptions: Vec<Subscription>,
}

/// The face's label as the two lines the editor shows; a third line or more
/// is folded into the second, which is the last one the key draws.
pub(super) fn label_lines(style: &KeyFaceStyle) -> [String; 2] {
    let label = style.label.as_deref().unwrap_or_default();
    match label.split_once('\n') {
        Some((first, rest)) => [first.to_owned(), rest.replace('\n', " ")],
        None => [label.to_owned(), String::new()],
    }
}

/// The label two line inputs make: `None` when both are blank, one line
/// when the second is.
pub(super) fn joined_label(first: &str, second: &str) -> Option<String> {
    match (first.trim(), second.trim()) {
        ("", "") => None,
        (first, "") => Some(first.to_owned()),
        ("", second) => Some(second.to_owned()),
        (first, second) => Some(format!("{first}\n{second}")),
    }
}

/// Whether the agent can draw `path` on a key: a PNG or JPEG file, by its
/// extension.
pub(super) fn is_key_face_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ["png", "jpg", "jpeg"]
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

impl FunctionRowView {
    /// Make sure the label inputs edit `slot`, seeding them from its saved
    /// face when they were made for another key (or none).
    pub(super) fn ensure_face_draft(
        &mut self,
        slot: GridSlot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.face.as_ref().is_some_and(|draft| draft.slot == slot) {
            return;
        }
        let style = AppState::try_read(cx)
            .map(|state| state.current_key_face(slot))
            .unwrap_or_default();
        let [first, second] = label_lines(&style);
        let line = |seed: String,
                    placeholder: SharedString,
                    window: &mut Window,
                    cx: &mut Context<Self>| {
            let input = cx.new(|cx| {
                let mut input = InputState::new(window, cx).placeholder(placeholder);
                if !seed.is_empty() {
                    input.set_value(seed, window, cx);
                }
                input
            });
            let subscription = cx.subscribe(&input, |view, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    view.commit_face_label(cx);
                }
            });
            (input, subscription)
        };
        let (first, first_sub) = line(first, tr!("keyboard.key_label_first_line"), window, cx);
        let (second, second_sub) = line(second, tr!("keyboard.key_label_second_line"), window, cx);
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("keyboard.key_symbol_search")));
        let search_sub = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let emoji_seed = match &style.symbol {
            Some(KeySymbol::Emoji(text)) => text.clone(),
            Some(KeySymbol::Lucide(_)) | None => String::new(),
        };
        let emoji = cx.new(|cx| {
            let mut input = InputState::new(window, cx)
                .placeholder(tr!("keyboard.key_symbol_emoji_placeholder"));
            if !emoji_seed.is_empty() {
                input.set_value(emoji_seed, window, cx);
            }
            input
        });
        let emoji_sub = cx.subscribe(&emoji, |view, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                view.commit_face_emoji(cx);
            }
        });
        let mut subscriptions = vec![first_sub, second_sub, search_sub, emoji_sub];
        let hex = ColorField::ALL.map(|field| {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(tr!("keyboard.key_color_custom"))
            });
            subscriptions.push(
                cx.subscribe(&input, move |view, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        view.commit_face_hex(field, cx);
                        cx.notify();
                    }
                }),
            );
            input
        });
        self.face = Some(FaceDraft {
            slot,
            lines: [first, second],
            search,
            emoji,
            symbol_mode: SymbolMode::of(style.symbol.as_ref()),
            hex,
            _subscriptions: subscriptions,
        });
    }

    /// Save the label the two line inputs hold.
    fn commit_face_label(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = &self.face else { return };
        let slot = draft.slot;
        let [first, second] = &draft.lines;
        let label = joined_label(&first.read(cx).value(), &second.read(cx).value());
        AppState::apply(cx, |state| {
            let style = KeyFaceStyle {
                label,
                ..state.current_key_face(slot)
            };
            state.commit_key_face(slot, style)
        });
    }

    /// Ask for a picture and put it on `slot`'s face; a file the agent
    /// cannot draw is refused with a note instead.
    fn choose_face_image(slot: GridSlot, cx: &mut Context<Self>) {
        let selected = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr!("keyboard.key_choose_image")),
        });
        cx.spawn(async move |view, cx| {
            let Ok(Ok(Some(paths))) = selected.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = view.update(cx, |view, cx| {
                view.face_image_rejected = !is_key_face_image(&path);
                if !view.face_image_rejected {
                    commit_face(cx, slot, |style| style.image = Some(path));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The Appearance tab for grid key `slot`, scrolling within the panel:
    /// with the icon grid open it is taller than the window allows.
    pub(super) fn appearance_panel(
        &self,
        slot: GridSlot,
        view: &Entity<Self>,
        pal: Palette,
        cx: &App,
    ) -> impl IntoElement {
        let style = AppState::try_read(cx)
            .map(|state| state.current_key_face(slot))
            .unwrap_or_default();
        let draft = self.face.as_ref().filter(|draft| draft.slot == slot);
        let symbol = draft.map(|draft| symbol_section(draft, style.symbol.as_ref(), view, pal, cx));
        let label = draft.map(|draft| {
            let [first, second] = &draft.lines;
            v_flex()
                .gap_1()
                .child(control_input(first))
                .child(control_input(second))
        });
        let icon = style.icon;
        let is_default = style.is_default();
        let view_for_reset = view.clone();

        let content = v_flex()
            .gap_1()
            .px_1()
            .pb_1()
            .child(editor_section(tr!("keyboard.key_symbol"), pal))
            .children(symbol)
            // Whether the action icon shows sits with the symbol that replaces it.
            .child(icon_row(slot, icon, pal))
            .child(editor_section(tr!("keyboard.key_label"), pal))
            .children(label.map(|label| div().px_2().child(label)))
            .child(editor_section(tr!("keyboard.key_background"), pal))
            .children(draft.map(|draft| {
                background_section(slot, &style, &draft.hex, self.face_hex_invalid, pal, cx)
            }))
            // A picture fills the key, so its background shows only behind
            // the label band; say so instead of letting a pick look ignored.
            .when(style.image.is_some(), |column| {
                column.child(
                    div()
                        .px_2()
                        .text_caption()
                        .text_color(pal.text_danger)
                        .child(tr!("keyboard.key_background_behind_image")),
                )
            })
            .children(draft.map(|draft| {
                text_and_icon_colors(slot, &style, &draft.hex, self.face_hex_invalid, pal)
            }))
            .child(editor_section(tr!("keyboard.key_image"), pal))
            .child(image_section(
                slot,
                style.image.as_deref(),
                self.face_image_rejected,
                view,
                pal,
            ))
            .child(
                div().px_1().pt_1().child(
                    Button::new("key-face-reset")
                        .ghost()
                        .small()
                        .label(tr!("keyboard.key_reset_appearance"))
                        .disabled(is_default)
                        .on_click(move |_, _, cx| {
                            AppState::apply(cx, |state| {
                                state.commit_key_face(slot, KeyFaceStyle::default())
                            });
                            // The label went too: reseed the inputs from the
                            // reset face on the next render.
                            view_for_reset.update(cx, |view, cx| {
                                view.face = None;
                                cx.notify();
                            });
                        }),
                ),
            );
        div()
            .id("key-face-scroll")
            .max_h(px(APPEARANCE_MAX_H))
            .overflow_y_scroll()
            .child(content)
    }
}

/// The picture's name, and the buttons that choose or remove it.
fn image_section(
    slot: GridSlot,
    image: Option<&Path>,
    rejected: bool,
    view: &Entity<FunctionRowView>,
    pal: Palette,
) -> gpui::Div {
    let view = view.clone();
    v_flex()
        .px_2()
        .gap_1()
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(image.map_or_else(|| tr!("keyboard.key_no_image"), file_name)),
        )
        .child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(
                    Button::new("key-face-choose-image")
                        .outline()
                        .small()
                        .label(tr!("keyboard.key_choose_image"))
                        .on_click(move |_, _, cx| {
                            view.update(cx, |_, cx| FunctionRowView::choose_face_image(slot, cx));
                        }),
                )
                .child(
                    Button::new("key-face-remove-image")
                        .ghost()
                        .small()
                        .label(tr!("keyboard.key_remove_image"))
                        .disabled(image.is_none())
                        .on_click(move |_, _, cx| {
                            commit_face(cx, slot, |style| style.image = None);
                        }),
                ),
        )
        .when(rejected, |column| {
            column.child(
                div()
                    .debug_selector(|| "key-face-image-rejected".into())
                    .text_caption()
                    .text_color(pal.text_danger)
                    .child(tr!("keyboard.key_image_unsupported")),
            )
        })
}

/// Whether the key draws its action's icon.
fn icon_row(slot: GridSlot, icon: bool, pal: Palette) -> gpui::Div {
    h_flex()
        .px_2()
        .pt_2()
        .justify_between()
        .items_center()
        .child(
            div()
                .text_body()
                .text_color(pal.text_primary)
                .child(tr!("keyboard.key_show_icon")),
        )
        .child(
            div().debug_selector(|| "key-face-icon".into()).child(
                Toggle::new("key-face-icon")
                    .accessibility_label(tr!("keyboard.key_show_icon"))
                    .selected(icon)
                    .on_change(move |icon, _window, cx| {
                        let icon = *icon;
                        commit_face(cx, slot, |style| style.icon = icon);
                    }),
            ),
        )
}

/// Change one thing about `slot`'s saved face.
pub(super) fn commit_face(cx: &mut App, slot: GridSlot, change: impl FnOnce(&mut KeyFaceStyle)) {
    AppState::apply(cx, |state| {
        let mut style = state.current_key_face(slot);
        change(&mut style);
        state.commit_key_face(slot, style)
    });
}

/// The picture's file name, for the line that says which one the key shows.
fn file_name(path: &Path) -> SharedString {
    path.file_name()
        .map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
        .into()
}

/// The panel's tabs for a grid key: what it does, and how it looks.
pub(super) fn panel_tabs(
    tab: KeyPanelTab,
    view: &Entity<FunctionRowView>,
    cx: &App,
) -> impl IntoElement {
    let view = view.clone();
    let tabs = [KeyPanelTab::Action, KeyPanelTab::Appearance];
    ButtonGroup::new("key-panel-tabs")
        .outline()
        .small()
        .children(tabs.iter().map(|candidate| {
            Button::new(match candidate {
                KeyPanelTab::Action => "key-panel-tab-action",
                KeyPanelTab::Appearance => "key-panel-tab-appearance",
            })
            .label(match candidate {
                KeyPanelTab::Action => tr!("keyboard.key_tab_action"),
                KeyPanelTab::Appearance => tr!("keyboard.key_appearance"),
            })
            .selected(*candidate == tab)
            .selected_segment(*candidate == tab, cx)
        }))
        .on_click(move |indices: &Vec<usize>, _, cx| {
            if let Some(tab) = indices.first().and_then(|index| tabs.get(*index)) {
                view.update(cx, |view, cx| view.set_panel_tab(*tab, cx));
            }
        })
}
