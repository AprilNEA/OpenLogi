//! The symbol a keypad's LCD key shows in place of its action's icon: a
//! [Lucide](https://lucide.dev) icon picked from the catalog
//! [`openlogi_ui::lucide`] ships, or an emoji. The key-appearance tab's
//! Symbol section, and the small pure rules behind its search.

use crate::ui::theme::FocusRingStyle as _;
use std::sync::OnceLock;

use gpui::{
    App, Entity, InteractiveElement, IntoElement, ParentElement, Role, SharedString,
    StatefulInteractiveElement as _, Styled, Toggled, div, prelude::FluentBuilder as _, px, svg,
};
use gpui_kit::base::Button as BaseButton;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex};
use openlogi_core::binding::GridSlot;
use openlogi_core::config::KeySymbol;
use openlogi_ui::lucide;

use super::FunctionRowView;
use super::key_face::{FaceDraft, commit_face};
use crate::ui::components::control_input;
use crate::ui::theme::{self, Palette, PressableStyle as _, SelectableStyle as _, Typography as _};

/// The most icons the picker lists for one query: enough to scan, few
/// enough to draw at once.
pub(super) const MAX_RESULTS: usize = 60;

/// What the picker offers before anything is typed: icons for the work a
/// creative keypad is bought for. Names the catalog lacks are skipped.
const SUGGESTED_ICONS: &[&str] = &[
    "camera",
    "mic",
    "play",
    "pause",
    "scissors",
    "brush",
    "save",
    "undo",
    "redo",
    "zoom-in",
    "zoom-out",
    "sun",
    "moon",
    "music",
    "video",
    "layers",
    "type",
    "crop",
    "copy",
    "clipboard",
    "trash",
    "star",
    "heart",
    "home",
];

/// A themed group of emojis in the picker, under a localized heading.
pub(super) struct EmojiGroup {
    /// The heading's catalog key.
    pub(super) key: &'static str,
    pub(super) emojis: &'static [&'static str],
}

/// The emojis offered one click away, grouped by theme (379 in all). Only
/// widely supported single emojis (Unicode 13 or older, no joined
/// sequences), so every desktop's colour emoji font draws them — on the
/// device as much as in the picker.
pub(super) const EMOJI_GROUPS: &[EmojiGroup] = &[
    EmojiGroup {
        key: "keyboard.emoji_group_smileys",
        emojis: &[
            "😀", "😃", "😄", "😁", "😆", "😅", "😂", "🤣", "😊", "😇", "🙂", "🙃", "😉", "😍",
            "🥰", "😘", "😎", "🤓", "🧐", "🤔", "🤨", "😐", "😶", "🙄", "😏", "😴", "🤯", "😱",
            "😡", "🤬", "😭", "🥳", "🤩", "😬", "🤐", "🥶", "🥵", "🤖", "👻", "💀", "👽", "💩",
        ],
    },
    EmojiGroup {
        key: "keyboard.emoji_group_gestures",
        emojis: &[
            "👍", "👎", "👌", "✌️", "🤞", "🤟", "🤘", "🤙", "👋", "👏", "🙌", "🙏", "💪", "👀",
            "👂", "🧠", "✍️", "👆", "👇", "👈", "👉", "☝️", "✋", "🖐️", "🤝",
        ],
    },
    EmojiGroup {
        key: "keyboard.emoji_group_media",
        emojis: &[
            "▶️", "⏸️", "⏯️", "⏹️", "⏺️", "⏭️", "⏮️", "⏩", "⏪", "🔀", "🔁", "🔂", "🔊", "🔉",
            "🔈", "🔇", "🎵", "🎶", "🎧", "🎤", "🎙️", "🎚️", "🎛️", "📻", "🎸", "🎹", "🥁", "🎺",
            "🎷", "🎻", "📺", "🎬", "🎞️", "📽️", "🎥", "📹", "📷", "📸",
        ],
    },
    EmojiGroup {
        key: "keyboard.emoji_group_creative",
        emojis: &[
            "🎨", "🖌️", "🖍️", "✏️", "✒️", "🖊️", "🖋️", "📐", "📏", "✂️", "🧵", "🧶", "🪄", "🖼️",
            "🌈", "💡", "🔦", "🕯️", "🎭", "🧩", "🎲", "🎯", "🧲", "🔮",
        ],
    },
    EmojiGroup {
        key: "keyboard.emoji_group_office",
        emojis: &[
            "📁", "📂", "🗂️", "📄", "📃", "📑", "📝", "📋", "📌", "📍", "📎", "🖇️", "🗃️", "🗄️",
            "🗑️", "💾", "💿", "📀", "🖨️", "✉️", "📧", "📨", "📩", "📤", "📥", "📦", "🏷️", "🔖",
            "📅", "📆", "🗓️", "📊", "📈", "📉", "🧾",
        ],
    },
    EmojiGroup {
        key: "keyboard.emoji_group_tech",
        emojis: &[
            "💻", "🖥️", "⌨️", "🖱️", "🖲️", "📱", "☎️", "📞", "🔋", "🔌", "💽", "📡", "🛰️", "🔍",
            "🔎", "🔒", "🔓", "🔑", "🗝️", "🔐", "⚙️", "🛠️", "🔧", "🔨", "🪛", "🧰", "🧪", "🔬",
            "🔭",
        ],
    },
    EmojiGroup {
        key: "keyboard.emoji_group_arrows",
        emojis: &[
            "⬆️", "⬇️", "⬅️", "➡️", "↗️", "↘️", "↙️", "↖️", "↕️", "↔️", "↩️", "↪️", "⤴️", "⤵️",
            "🔄", "🔃", "🔙", "🔚", "🔛", "🔜", "🔝", "➕", "➖", "✖️", "➗", "♾️", "✅", "☑️",
            "✔️", "❌", "❎", "⚠️", "🚫", "⛔", "❓", "❗", "🔴", "🟠", "🟡", "🟢", "🔵", "🟣",
            "⚫", "⚪", "🟥", "🟧", "🟨", "🟩", "🟦", "🟪", "⬛", "⬜",
        ],
    },
    EmojiGroup {
        key: "keyboard.emoji_group_nature",
        emojis: &[
            "☀️", "🌤️", "⛅", "☁️", "🌧️", "⛈️", "🌩️", "❄️", "☃️", "🌙", "🌕", "⭐", "🌟", "✨",
            "⚡", "🔥", "💧", "🌊", "🌪️", "🌍", "🌲", "🌳", "🌴", "🌵", "🌷", "🌹", "🌻", "🍀",
            "🍁", "🍂", "🐶", "🐱", "🦊", "🐻", "🐼", "🦁", "🐸", "🐵", "🦄", "🐝", "🦋", "🐢",
            "🐙", "🐬",
        ],
    },
    EmojiGroup {
        key: "keyboard.emoji_group_food",
        emojis: &[
            "☕", "🍵", "🍺", "🍷", "🥤", "🍕", "🍔", "🍟", "🌭", "🌮", "🍣", "🍜", "🍝", "🥗",
            "🍎", "🍌", "🍓", "🍒", "🍉", "🍇", "🥑", "🍩", "🍪", "🎂", "🍰", "🍫", "🍿",
        ],
    },
    EmojiGroup {
        key: "keyboard.emoji_group_activities",
        emojis: &[
            "⚽", "🏀", "🏈", "🎾", "🏐", "🎮", "🕹️", "🏆", "🥇", "🏁", "🚩", "🎉", "🎊", "🎁",
            "🎈", "🚀", "✈️", "🚗", "🚲", "🚂", "🏠", "🏢", "🏖️", "⛰️", "🗺️", "🧭", "⏰", "⏱️",
            "⏳", "⌛", "🕐",
        ],
    },
    EmojiGroup {
        key: "keyboard.emoji_group_symbols",
        emojis: &[
            "❤️", "🧡", "💛", "💚", "💙", "💜", "🖤", "🤍", "🤎", "💔", "❣️", "💕", "💖", "💯",
            "💢", "💥", "💫", "💬", "💭", "🗯️", "💤", "♻️", "⚛️", "☮️", "☯️", "✳️", "❇️", "🆗",
            "🆕", "🆒", "🆓", "🆘",
        ],
    },
];

/// Every emoji the picker offers, in display order.
#[cfg(test)]
pub(super) fn emoji_presets() -> impl Iterator<Item = &'static str> {
    EMOJI_GROUPS
        .iter()
        .flat_map(|group| group.emojis.iter().copied())
}

const CELL: f32 = 30.;
const CELL_ICON: f32 = 20.;
/// Height the icon grid scrolls within.
const GRID_MAX_H: f32 = 150.;
/// Height the grouped emoji grid scrolls within.
const EMOJI_GRID_MAX_H: f32 = 240.;

/// Which kind of symbol the section is choosing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SymbolMode {
    /// No symbol: the key shows its action's icon.
    #[default]
    ActionIcon,
    Lucide,
    Emoji,
}

impl SymbolMode {
    /// The mode that shows `symbol`.
    pub(super) fn of(symbol: Option<&KeySymbol>) -> Self {
        match symbol {
            None => Self::ActionIcon,
            Some(KeySymbol::Lucide(_)) => Self::Lucide,
            Some(KeySymbol::Emoji(_)) => Self::Emoji,
        }
    }
}

/// The Lucide catalog's names, read once.
fn catalog() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(lucide::names)
}

/// The suggestions [`SUGGESTED_ICONS`] names that `catalog` has, in order.
pub(super) fn suggested_icons(catalog: &[String]) -> Vec<String> {
    SUGGESTED_ICONS
        .iter()
        .filter(|name| catalog.iter().any(|known| known == *name))
        .map(|name| (*name).to_owned())
        .collect()
}

/// The icons of `catalog` matching `query`, at most `cap` of them: names
/// containing the query (case and surrounding space ignored), the exact
/// name first, then names starting with it, then the rest, each group
/// alphabetical. A blank query lists the suggestions.
pub(super) fn search_icons(catalog: &[String], query: &str, cap: usize) -> Vec<String> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        let mut suggested = suggested_icons(catalog);
        suggested.truncate(cap);
        return suggested;
    }
    let mut matches: Vec<&String> = catalog
        .iter()
        .filter(|name| name.contains(&query))
        .collect();
    matches.sort_by_key(|name| {
        let rank = if **name == query {
            0
        } else if name.starts_with(&query) {
            1
        } else {
            2
        };
        (rank, (*name).clone())
    });
    matches.into_iter().take(cap).cloned().collect()
}

/// The emoji symbol typed `text` makes, or `None` when nothing is left once
/// it is trimmed (see [`KeySymbol::normalized`]).
pub(super) fn emoji_symbol(text: &str) -> Option<KeySymbol> {
    KeySymbol::Emoji(text.to_owned()).normalized()
}

impl FunctionRowView {
    /// Show `mode`'s chooser for the face being edited. Choosing the action
    /// icon is itself a pick: it removes the symbol.
    fn set_symbol_mode(&mut self, slot: GridSlot, mode: SymbolMode, cx: &mut App) {
        if let Some(draft) = self.face.as_mut().filter(|draft| draft.slot == slot) {
            draft.symbol_mode = mode;
        }
        if mode == SymbolMode::ActionIcon {
            commit_face(cx, slot, |style| style.symbol = None);
        }
    }

    /// Save the emoji the input holds, once it holds one.
    pub(super) fn commit_face_emoji(&mut self, cx: &mut App) {
        let Some(draft) = &self.face else { return };
        let slot = draft.slot;
        if let Some(symbol) = emoji_symbol(&draft.emoji.read(cx).value()) {
            commit_face(cx, slot, |style| style.symbol = Some(symbol));
        }
    }
}

/// The Symbol section for `slot`: the mode, its chooser, and the action
/// that removes the symbol.
pub(super) fn symbol_section(
    draft: &FaceDraft,
    symbol: Option<&KeySymbol>,
    view: &Entity<FunctionRowView>,
    pal: Palette,
    cx: &App,
) -> gpui::Div {
    let slot = draft.slot;
    let mode = draft.symbol_mode;
    let chooser = match mode {
        SymbolMode::ActionIcon => None,
        SymbolMode::Lucide => Some(lucide_chooser(draft, symbol, pal, cx)),
        SymbolMode::Emoji => Some(emoji_chooser(draft, symbol, pal)),
    };
    v_flex()
        .px_2()
        .gap_2()
        .child(mode_tabs(slot, mode, view, cx))
        .children(chooser)
        .when(symbol.is_some(), |column| {
            column.child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("keyboard.key_symbol_replaces_icon")),
            )
        })
        .child(
            div().child(
                Button::new("key-symbol-remove")
                    .ghost()
                    .small()
                    .label(tr!("keyboard.key_remove_symbol"))
                    .disabled(symbol.is_none())
                    .on_click({
                        let view = view.clone();
                        move |_, _, cx| {
                            view.update(cx, |view, cx| {
                                view.set_symbol_mode(slot, SymbolMode::ActionIcon, cx);
                                cx.notify();
                            });
                        }
                    }),
            ),
        )
}

/// The three kinds of symbol, as one segmented control.
fn mode_tabs(
    slot: GridSlot,
    mode: SymbolMode,
    view: &Entity<FunctionRowView>,
    cx: &App,
) -> impl IntoElement {
    let view = view.clone();
    let modes = [
        SymbolMode::ActionIcon,
        SymbolMode::Lucide,
        SymbolMode::Emoji,
    ];
    ButtonGroup::new("key-symbol-modes")
        .outline()
        .small()
        .children(modes.iter().map(|candidate| {
            let (id, label) = match candidate {
                SymbolMode::ActionIcon => {
                    ("key-symbol-action", tr!("keyboard.key_symbol_action_icon"))
                }
                SymbolMode::Lucide => ("key-symbol-lucide", tr!("keyboard.key_symbol_lucide")),
                SymbolMode::Emoji => ("key-symbol-emoji", tr!("keyboard.key_symbol_emoji")),
            };
            Button::new(id)
                .label(label)
                .selected(*candidate == mode)
                .selected_segment(*candidate == mode, cx)
        }))
        .on_click(move |indices: &Vec<usize>, _, cx| {
            if let Some(mode) = indices.first().and_then(|index| modes.get(*index)) {
                view.update(cx, |view, cx| {
                    view.set_symbol_mode(slot, *mode, cx);
                    cx.notify();
                });
            }
        })
}

/// The icon search and the grid of what it finds, the key's icon selected.
fn lucide_chooser(
    draft: &FaceDraft,
    symbol: Option<&KeySymbol>,
    pal: Palette,
    cx: &App,
) -> gpui::Div {
    let slot = draft.slot;
    let current = match symbol {
        Some(KeySymbol::Lucide(name)) => Some(name.as_str()),
        Some(KeySymbol::Emoji(_)) | None => None,
    };
    let results = search_icons(catalog(), &draft.search.read(cx).value(), MAX_RESULTS);
    let grid = if results.is_empty() {
        div()
            .text_caption()
            .text_color(pal.text_muted)
            .child(tr!("keyboard.key_symbol_no_match"))
            .into_any_element()
    } else {
        div()
            .id("key-symbol-grid")
            .debug_selector(|| "key-symbol-grid".into())
            .max_h(px(GRID_MAX_H))
            .overflow_y_scroll()
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_1()
                    .children(results.into_iter().map(|name| {
                        let selected = current == Some(name.as_str());
                        let path = lucide::asset_path(&name);
                        let symbol = KeySymbol::Lucide(name.clone());
                        symbol_cell(
                            SharedString::from(format!("key-symbol-{name}")),
                            selected,
                            name.into(),
                            pal,
                        )
                        .child(
                            svg()
                                .path(path)
                                .size(px(CELL_ICON))
                                .text_color(if selected {
                                    theme::accent()
                                } else {
                                    pal.text_primary
                                }),
                        )
                        .on_click(move |_, _, cx| {
                            let symbol = symbol.clone();
                            commit_face(cx, slot, |style| style.symbol = Some(symbol));
                        })
                    })),
            )
            .into_any_element()
    };
    v_flex()
        .gap_1p5()
        .child(control_input(&draft.search))
        .child(grid)
}

/// The emoji input and the grouped presets, the key's emoji selected.
fn emoji_chooser(draft: &FaceDraft, symbol: Option<&KeySymbol>, pal: Palette) -> gpui::Div {
    let slot = draft.slot;
    let current = match symbol {
        Some(KeySymbol::Emoji(text)) => Some(text.as_str()),
        Some(KeySymbol::Lucide(_)) | None => None,
    };
    let mut index = 0usize;
    let groups = EMOJI_GROUPS.iter().map(|group| {
        let cells = group
            .emojis
            .iter()
            .map(|&emoji| {
                let id = SharedString::from(format!("key-symbol-emoji-{index}"));
                index += 1;
                symbol_cell(id, current == Some(emoji), emoji.into(), pal)
                    .child(div().text_base().child(emoji))
                    .on_click(move |_, _, cx| {
                        let symbol = KeySymbol::Emoji(emoji.to_owned());
                        commit_face(cx, slot, |style| style.symbol = Some(symbol));
                    })
            })
            .collect::<Vec<_>>();
        v_flex()
            .gap_1()
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!(group.key)),
            )
            .child(h_flex().flex_wrap().gap_1().children(cells))
    });
    v_flex().gap_1p5().child(control_input(&draft.emoji)).child(
        div()
            .id("key-symbol-emoji-presets")
            .debug_selector(|| "key-symbol-emoji-presets".into())
            .max_h(px(EMOJI_GRID_MAX_H))
            .overflow_y_scroll()
            .child(v_flex().gap_2().children(groups)),
    )
}

/// One choosable symbol: a square cell, outlined in the accent when it is
/// the key's.
fn symbol_cell(id: SharedString, selected: bool, label: SharedString, pal: Palette) -> BaseButton {
    BaseButton::new(id)
        .role(Role::RadioButton)
        .selected(selected)
        .accessibility_label(label.clone())
        .aria_toggled(if selected {
            Toggled::True
        } else {
            Toggled::False
        })
        .aria_selected(selected)
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(label.clone()).build(window, cx)
        })
        .size(px(CELL))
        .flex()
        .items_center()
        .justify_center()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(if selected {
            theme::accent()
        } else {
            pal.border
        })
        .when(selected, |cell| cell.bg(theme::accent_tint()))
        .pressable_fill(selected, pal)
        .cursor_pointer()
        .focus_visible(move |style| style.focus_ring(pal))
}
