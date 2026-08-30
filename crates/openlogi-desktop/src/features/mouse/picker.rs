//! Shared action-catalog rows and menu surfaces used by binding editors.

use std::rc::Rc;

use gpui::{
    App, InteractiveElement, IntoElement, ParentElement, Role, StatefulInteractiveElement as _,
    Styled, Window, div, prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_component::{Icon, IconName, Selectable as _, h_flex, v_flex};
use openlogi_core::binding::{Action, ActionRingIcon, Category, GestureDirection};

use crate::ui::components::MenuRow;
use crate::ui::section::section_label;
use crate::ui::theme::{ACCENT_BLUE, Palette, Typography as _};

/// Height cap shared by compact inspector and editor lists.
pub(crate) const EDITOR_LIST_MAX_H: f32 = 360.;

/// Commit callback invoked when an action row is clicked.
pub(crate) type PickFn = Rc<dyn Fn(Action, &mut Window, &mut App)>;

/// Which bindings a catalog is allowed to offer.
///
/// Hold-mode actions need a press that stays down. A swipe slot, function key,
/// or ring tap cannot keep that hold alive, so those surfaces use
/// [`ActionCatalogKind::Instant`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActionCatalogKind {
    /// A physical button: hold-mode pan and zoom are offered here.
    Button,
    /// A tap, keypress, or swipe: hold-mode actions cannot fire.
    Instant,
}

impl ActionCatalogKind {
    fn admits(self, action: &Action) -> bool {
        match self {
            Self::Button => true,
            Self::Instant => !action.is_hold_mode(),
        }
    }
}

/// The action catalog grouped by [`Category`], preserving catalog order within
/// each group and first-seen order across groups.
pub(crate) fn grouped_catalog(kind: ActionCatalogKind) -> Vec<(Category, Vec<Action>)> {
    let mut sections: Vec<(Category, Vec<Action>)> = Vec::new();
    for action in Action::catalog() {
        if !kind.admits(&action) {
            continue;
        }
        let category = action.category();
        if let Some(section) = sections
            .iter_mut()
            .find(|(existing, _)| *existing == category)
        {
            section.1.push(action);
        } else {
            sections.push((category, vec![action]));
        }
    }
    sections
}

/// Icon for a gesture-mode button's five-direction summary.
pub(crate) const GESTURE_BUTTON_ICON: &str = "action-icons/move.svg";

/// Icon for one gesture direction: an arrow away from the centre, or the
/// centre itself for the click. Four come from gpui-component's bundled set
/// and the dot is vendored, but all five are lucide at the same stroke weight
/// as the action icons they sit above.
pub(crate) fn gesture_direction_icon(direction: GestureDirection) -> Icon {
    match direction {
        GestureDirection::Up => Icon::new(IconName::ArrowUp),
        GestureDirection::Down => Icon::new(IconName::ArrowDown),
        GestureDirection::Left => Icon::new(IconName::ArrowLeft),
        GestureDirection::Right => Icon::new(IconName::ArrowRight),
        GestureDirection::Click => Icon::empty().path("action-icons/circle-dot.svg"),
    }
}

/// Asset path of the vendored glyph for an action. Exhaustive so a new action
/// must deliberately choose an icon.
pub(crate) fn action_icon_path(action: &Action) -> &'static str {
    match action {
        Action::None => "action-icons/ban.svg",
        Action::LeftClick | Action::RightClick => "action-icons/mouse-pointer-click.svg",
        Action::MiddleClick => "action-icons/mouse.svg",
        Action::MouseBack => "action-icons/circle-arrow-left.svg",
        Action::MouseForward => "action-icons/circle-arrow-right.svg",
        Action::Copy => "action-icons/copy.svg",
        Action::Paste => "action-icons/clipboard-paste.svg",
        Action::Cut => "action-icons/scissors.svg",
        Action::Undo => "action-icons/undo-2.svg",
        Action::Redo => "action-icons/redo-2.svg",
        Action::SelectAll | Action::Workflow(_) => "action-icons/list-checks.svg",
        Action::Find => "action-icons/search.svg",
        Action::Save => "action-icons/save.svg",
        Action::BrowserBack => "action-icons/arrow-left.svg",
        Action::BrowserForward => "action-icons/arrow-right.svg",
        Action::NewTab => "action-icons/square-plus.svg",
        Action::CloseTab => "action-icons/square-x.svg",
        Action::ReopenTab => "action-icons/rotate-ccw.svg",
        Action::NextTab => "action-icons/chevron-right.svg",
        Action::PrevTab => "action-icons/chevron-left.svg",
        Action::ReloadPage => "action-icons/rotate-cw.svg",
        Action::MissionControl | Action::ShowActionsRing => "action-icons/layout-grid.svg",
        Action::AppExpose => "action-icons/layers.svg",
        Action::PreviousDesktop => "action-icons/square-arrow-left.svg",
        Action::NextDesktop => "action-icons/square-arrow-right.svg",
        Action::ShowDesktop => "action-icons/monitor.svg",
        Action::LaunchpadShow | Action::OpenApplication(_) => "action-icons/grid-3x3.svg",
        Action::LockScreen => "action-icons/lock.svg",
        Action::Screenshot | Action::CaptureRegion => "action-icons/camera.svg",
        Action::Sleep => "action-icons/moon.svg",
        Action::PlayPause => "action-icons/play.svg",
        Action::NextTrack => "action-icons/skip-forward.svg",
        Action::PrevTrack => "action-icons/skip-back.svg",
        Action::VolumeUp => "action-icons/volume-2.svg",
        Action::VolumeDown => "action-icons/volume-1.svg",
        Action::MuteVolume => "action-icons/volume-x.svg",
        Action::CycleDpiPresets | Action::SetDpiPreset(_) => "action-icons/gauge.svg",
        Action::ToggleSmartShift => "action-icons/refresh-cw.svg",
        Action::ScrollUp => "action-icons/chevrons-up.svg",
        Action::ScrollDown => "action-icons/chevrons-down.svg",
        Action::HorizontalScrollLeft => "action-icons/chevrons-left.svg",
        Action::HorizontalScrollRight => "action-icons/chevrons-right.svg",
        Action::CustomShortcut(_) | Action::HoldShortcut(_) | Action::TypeText(_) => {
            "action-icons/keyboard.svg"
        }
        Action::RunAppleScript(_) | Action::RunShellCommand(_) => "action-icons/terminal.svg",
        // Same glyphs the ring registry assigns — a hand-edited binding must
        // not show a different icon than the picker row that offered it.
        Action::Pan | Action::Zoom => ActionRingIcon::for_action(action).asset_path(),
    }
}

/// Host-honest picker caption for a hold-mode action.
///
/// macOS can deliver a phased trackpad pan and a real pinch; Linux and
/// Windows degrade to wheel ticks and Ctrl+wheel, so those hosts must not
/// promise rubber-band, momentum, or pinch.
pub(crate) fn hold_mode_hint(action: &Action) -> Option<&'static str> {
    match action {
        Action::Pan if cfg!(target_os = "macos") => {
            Some("Hold and drag to scroll in any direction.")
        }
        Action::Pan => Some("Hold and drag to scroll."),
        Action::Zoom if cfg!(target_os = "macos") => {
            Some("Hold and drag up or down to pinch-zoom.")
        }
        Action::Zoom => Some("Hold and drag to Ctrl+zoom."),
        _ => None,
    }
}

/// Build every category-grouped action row.
pub(crate) fn action_rows(
    id_prefix: &'static str,
    current: Option<&Action>,
    kind: ActionCatalogKind,
    on_pick: &PickFn,
    pal: Palette,
) -> Vec<gpui::Div> {
    action_rows_matching(id_prefix, current, "", kind, on_pick, pal)
}

/// Build action rows filtered by localized action or category name.
pub(crate) fn action_rows_matching(
    id_prefix: &'static str,
    current: Option<&Action>,
    query: &str,
    kind: ActionCatalogKind,
    on_pick: &PickFn,
    pal: Palette,
) -> Vec<gpui::Div> {
    let query = query.trim().to_lowercase();
    let mut catalog_index = 0usize;
    let mut sections = Vec::new();
    for (category, actions) in grouped_catalog(kind) {
        let category_label = rust_i18n::t!(category.label());
        let category_matches = category_label.to_lowercase().contains(&query);
        // Number the full catalog before filtering so typing in the search box
        // never changes an action row's element identity.
        let actions: Vec<(usize, Action)> = actions
            .into_iter()
            .map(|action| {
                let key = catalog_index;
                catalog_index += 1;
                (key, action)
            })
            .filter(|(_, action)| {
                query.is_empty()
                    || category_matches
                    || rust_i18n::t!(action.label())
                        .to_lowercase()
                        .contains(&query)
                    || action.label().to_lowercase().contains(&query)
            })
            .collect();
        if actions.is_empty() {
            continue;
        }
        sections.push(
            v_flex()
                .child(editor_section(category_label.into_owned(), pal))
                .children(actions.into_iter().map(|(action_key, action)| {
                    let selected = current == Some(&action);
                    let label = tr!(action.label());
                    let hint = hold_mode_hint(&action);
                    let accessible_label = match hint {
                        Some(key) => format!("{label} — {}", rust_i18n::t!(key)).into(),
                        None => label.clone(),
                    };
                    let icon_path = action_icon_path(&action);
                    let on_pick = on_pick.clone();
                    MenuRow::new((id_prefix, action_key))
                        .selected(selected)
                        .role(Role::MenuItem)
                        .aria_label(accessible_label)
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    svg()
                                        .path(icon_path)
                                        .size_4()
                                        .flex_none()
                                        .text_color(pal.text_muted),
                                )
                                .child(v_flex().min_w_0().child(div().child(label)).children(
                                    hint.map(|key| {
                                        div()
                                            .text_caption()
                                            .text_color(pal.text_muted)
                                            .child(tr!(key))
                                    }),
                                )),
                        )
                        .when(selected, |row| {
                            row.child(
                                Icon::new(IconName::Check)
                                    .size_3()
                                    .text_color(rgb(ACCENT_BLUE)),
                            )
                        })
                        .on_click(move |_event, window, cx| (on_pick)(action.clone(), window, cx))
                })),
        );
    }
    sections
}

/// Shared card surface for compact binding panels and menus.
pub(crate) fn compact_panel(pal: Palette) -> gpui::Div {
    v_flex()
        .bg(pal.panel)
        .border_1()
        .border_color(pal.border)
        .rounded(pal.card_radius)
        .shadow_md()
        .p_1p5()
}

/// A group heading inset to line up with the rows under it.
pub(crate) fn editor_section(label: impl Into<gpui::SharedString>, pal: Palette) -> gpui::Div {
    section_label(label, pal).w_full().px_2().pt_2().pb_0p5()
}

/// Compact editor title.
pub(crate) fn title(text: impl Into<gpui::SharedString>, pal: Palette) -> impl IntoElement {
    div()
        .px_2()
        .pb_1()
        .text_subheading()
        .text_color(pal.text_muted)
        .child(text.into())
}

/// Hairline separating compact editor regions.
pub(crate) fn divider(pal: Palette) -> impl IntoElement {
    div().mb_1().h(px(1.)).w_full().bg(pal.border)
}

/// Height-capped scroll region for compact editor rows.
pub(crate) fn editor_scroll_list(
    id: &'static str,
    rows: impl IntoIterator<Item = impl IntoElement>,
) -> impl IntoElement {
    div()
        .id(id)
        .max_h(px(EDITOR_LIST_MAX_H))
        .overflow_y_scroll()
        .children(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_actions(kind: ActionCatalogKind) -> Vec<Action> {
        grouped_catalog(kind)
            .into_iter()
            .flat_map(|(_, actions)| actions)
            .collect()
    }

    #[test]
    fn button_catalog_offers_hold_mode() {
        let actions = catalog_actions(ActionCatalogKind::Button);
        assert!(actions.contains(&Action::Pan));
        assert!(actions.contains(&Action::Zoom));
        assert!(actions.contains(&Action::ScrollUp));
    }

    #[test]
    fn instant_catalog_omits_hold_mode() {
        let actions = catalog_actions(ActionCatalogKind::Instant);
        assert!(!actions.contains(&Action::Pan));
        assert!(!actions.contains(&Action::Zoom));
        assert!(
            !actions.iter().any(Action::is_hold_mode),
            "a swipe or keypress catalog leaked a hold-mode action"
        );
        assert!(actions.contains(&Action::ScrollUp));
        assert!(actions.contains(&Action::Copy));
    }

    #[test]
    fn hold_mode_icons_match_the_ring_registry() {
        assert_eq!(
            action_icon_path(&Action::Pan),
            ActionRingIcon::for_action(&Action::Pan).asset_path()
        );
        assert_eq!(
            action_icon_path(&Action::Zoom),
            ActionRingIcon::for_action(&Action::Zoom).asset_path()
        );
        assert_eq!(action_icon_path(&Action::Pan), "action-icons/mouse.svg");
        assert_eq!(action_icon_path(&Action::Zoom), "action-icons/search.svg");
    }

    #[test]
    fn hold_mode_hints_are_honest_on_this_host() {
        assert_eq!(hold_mode_hint(&Action::ScrollUp), None);
        assert_eq!(hold_mode_hint(&Action::Copy), None);
        if cfg!(target_os = "macos") {
            assert_eq!(
                hold_mode_hint(&Action::Pan),
                Some("Hold and drag to scroll in any direction.")
            );
            assert_eq!(
                hold_mode_hint(&Action::Zoom),
                Some("Hold and drag up or down to pinch-zoom.")
            );
        } else {
            assert_eq!(
                hold_mode_hint(&Action::Pan),
                Some("Hold and drag to scroll.")
            );
            assert_eq!(
                hold_mode_hint(&Action::Zoom),
                Some("Hold and drag to Ctrl+zoom.")
            );
        }
    }

    #[test]
    fn gesture_action_catalog_includes_actions_ring() {
        for kind in [ActionCatalogKind::Button, ActionCatalogKind::Instant] {
            assert!(
                catalog_actions(kind).contains(&Action::ShowActionsRing),
                "{kind:?} picker dropped the Actions Ring"
            );
        }
    }
}
