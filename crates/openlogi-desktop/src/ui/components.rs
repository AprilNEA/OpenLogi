//! Reusable, theme-aware desktop components.
//!
//! These components own their semantic state and resolve the active palette at
//! render time, so callers do not have to thread colours through free-function
//! builders.

use gpui::{
    AnyElement, App, ClickEvent, ElementId, Entity, InteractiveElement, Interactivity, IntoElement,
    ParentElement, Pixels, RenderOnce, Role, SharedString, Stateful, StatefulInteractiveElement,
    Styled, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    Disableable, Icon, IconName, Selectable, Sizable, Size, h_flex,
    input::{Input, InputState},
    searchable_list::SearchableListDelegate,
    select::{Select, SelectState},
    switch::Switch,
    v_flex,
};

use super::theme::{self, ACCENT_BLUE, SelectableStyle as _, Typography as _};

/// A gpui-component button at the house control height.
///
/// gpui-component's size ladder (20/24/32/44 px) has no step at the app's
/// 30 px control rhythm, and this rev's custom `Size::Size` is broken for
/// heights (`input_h` falls through to 24 px, and the text scales with the
/// size). Standalone form controls therefore take `.small()` typography and
/// pin [`theme::CONTROL_H`] explicitly — construct them through these
/// helpers; a bare `.small()` on one is a bug. Ghost and link affordances,
/// pills, and icon-only toggles stay on the stock ladder on purpose.
pub(crate) fn control_button(id: impl Into<ElementId>) -> gpui_component::button::Button {
    gpui_component::button::Button::new(id)
        .small()
        .h(px(theme::CONTROL_H))
}

/// A single-line input at the house control height; see [`control_button`].
/// Pins via `min_h` because the inherent `Input::h` is multi-line-only.
pub(crate) fn control_input(state: &Entity<InputState>) -> Input {
    Input::new(state).small().min_h(px(theme::CONTROL_H))
}

/// Re-derive an input's placeholder from the current locale.
///
/// Placeholders are stored inside [`InputState`] at construction, so a live
/// language switch leaves them stale — the owning view calls this from render
/// with a fresh `tr!` string to keep them current. Guarded, because
/// `set_placeholder` notifies unconditionally and a bare per-render call would
/// re-render forever.
pub(crate) fn localize_placeholder(
    state: &Entity<InputState>,
    text: SharedString,
    window: &mut Window,
    cx: &mut App,
) {
    if *state.read(cx).presentation().placeholder() != text {
        state.update(cx, |state, cx| state.set_placeholder(text, window, cx));
    }
}

/// A select trigger at the house control height; see [`control_button`].
pub(crate) fn control_select<D: SearchableListDelegate + 'static>(
    state: &Entity<SelectState<D>>,
) -> Select<D> {
    Select::new(state).small().min_h(px(theme::CONTROL_H))
}

/// A controlled on/off switch with a compact state label.
#[derive(IntoElement)]
pub(crate) struct Toggle {
    id: ElementId,
    selected: bool,
    disabled: bool,
    size: Size,
    label: Option<SharedString>,
    icon: Option<IconName>,
    min_width: Option<Pixels>,
    on_change: Option<SwitchHandler>,
}

type SwitchHandler = std::rc::Rc<dyn Fn(&bool, &mut Window, &mut App)>;

impl Toggle {
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            selected: false,
            disabled: false,
            size: Size::Small,
            label: None,
            icon: None,
            min_width: None,
            on_change: None,
        }
    }

    #[must_use]
    pub(crate) fn label(mut self, label: impl Into<Option<SharedString>>) -> Self {
        self.label = label.into();
        self
    }

    #[must_use]
    pub(crate) fn icon(mut self, icon: impl Into<Option<IconName>>) -> Self {
        self.icon = icon.into();
        self
    }

    #[must_use]
    pub(crate) fn min_width(mut self, width: impl Into<Option<Pixels>>) -> Self {
        self.min_width = width.into();
        self
    }

    #[must_use]
    pub(crate) fn on_change(
        mut self,
        handler: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(std::rc::Rc::new(handler));
        self
    }
}

impl Selectable for Toggle {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl Disableable for Toggle {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl Sizable for Toggle {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl RenderOnce for Toggle {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let label = self
            .label
            .unwrap_or_else(|| if self.selected { tr!("On") } else { tr!("Off") });
        let mut toggle = Switch::new(self.id)
            .checked(self.selected)
            .disabled(self.disabled)
            .with_size(self.size)
            .label(label)
            .color(rgb(ACCENT_BLUE));
        if let Some(handler) = self.on_change {
            toggle = toggle.on_click(move |selected, window, cx| handler(selected, window, cx));
        }
        h_flex()
            .items_center()
            .gap_2()
            .when_some(self.min_width, |row, width| {
                row.min_w(width).justify_center()
            })
            .when_some(self.icon, |row, icon| row.child(Icon::new(icon).size_3()))
            .child(toggle)
    }
}

/// A titled card used by device-detail panels.
#[derive(IntoElement)]
pub(crate) struct PanelCard {
    title: SharedString,
    icon: Icon,
    content: AnyElement,
    fill: bool,
}

impl PanelCard {
    pub(crate) fn new(
        title: impl Into<SharedString>,
        icon: Icon,
        content: impl IntoElement,
    ) -> Self {
        Self {
            title: title.into(),
            icon,
            content: content.into_any_element(),
            fill: false,
        }
    }

    #[must_use]
    pub(crate) fn fill(mut self) -> Self {
        self.fill = true;
        self
    }
}

impl RenderOnce for PanelCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        div()
            .w_full()
            .when(self.fill, gpui::Styled::h_full)
            .max_w_full()
            .min_w_0()
            .rounded(pal.card_radius)
            .border_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .p(theme::CARD_PAD.rems())
            .child(
                v_flex()
                    .gap(theme::CARD_GAP.rems())
                    .when(!self.title.is_empty(), |this| {
                        this.child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .text_color(pal.text_primary)
                                .child(self.icon.size_4().text_color(pal.text_muted))
                                .child(div().text_subheading().child(self.title)),
                        )
                    })
                    .child(self.content),
            )
    }
}

type ClickHandler = std::rc::Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A full-width row in an action menu.
#[derive(IntoElement)]
pub(crate) struct MenuRow {
    base: BaseButton,
    selected: bool,
    children: Vec<AnyElement>,
    on_click: Option<ClickHandler>,
}

impl MenuRow {
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: BaseButton::new(id),
            selected: false,
            children: Vec::new(),
            on_click: None,
        }
    }

    /// Set the accessibility role for this row's semantic button.
    #[must_use]
    pub(crate) fn role(mut self, role: Role) -> Self {
        self.base = self.base.role(role);
        self
    }

    #[must_use]
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(std::rc::Rc::new(handler));
        self
    }
}

impl Selectable for MenuRow {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl ParentElement for MenuRow {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl InteractiveElement for MenuRow {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl StatefulInteractiveElement for MenuRow {}

impl RenderOnce for MenuRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        let selected = self.selected;
        self.base
            .selected(selected)
            .aria_selected(selected)
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded(pal.control_radius)
            .text_body()
            .text_color(pal.text_primary)
            .selected_fill(selected)
            .hover(move |style| {
                style.bg(if selected {
                    theme::accent_tint_hover()
                } else {
                    pal.control_hover
                })
            })
            .focus_visible(move |style| {
                style.bg(if selected {
                    theme::accent_tint_hover()
                } else {
                    pal.control_hover
                })
            })
            .children(self.children)
            .when_some(self.on_click, |row, handler| {
                row.on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
}

/// A selectable camera-profile chip.
#[derive(IntoElement)]
pub(crate) struct ProfileTab {
    base: BaseButton,
    label: SharedString,
    icon: Option<Icon>,
    selected: bool,
    children: Vec<AnyElement>,
    on_click: Option<ClickHandler>,
    delete: Option<(ElementId, ClickHandler)>,
}

impl ProfileTab {
    pub(crate) fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            base: BaseButton::new(id).role(Role::Tab),
            label: label.into(),
            icon: None,
            selected: false,
            children: Vec::new(),
            on_click: None,
            delete: None,
        }
    }

    /// A mark before the label, for a tab that is an action rather than a
    /// profile (the one that creates a new profile).
    #[must_use]
    pub(crate) fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    #[must_use]
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(std::rc::Rc::new(handler));
        self
    }

    #[must_use]
    pub(crate) fn on_delete(
        mut self,
        id: impl Into<ElementId>,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.delete = Some((id.into(), std::rc::Rc::new(handler)));
        self
    }
}

impl Selectable for ProfileTab {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl ParentElement for ProfileTab {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl InteractiveElement for ProfileTab {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl StatefulInteractiveElement for ProfileTab {}

impl RenderOnce for ProfileTab {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        let accent = rgb(ACCENT_BLUE);
        let has_delete = self.delete.is_some();
        let label = self.label;
        self.base
            .selected(self.selected)
            .aria_selected(self.selected)
            .accessibility_label(label.clone())
            .when(!has_delete, gpui::Styled::px_2)
            .when(has_delete, |tab| tab.pl_2().pr_1())
            .flex()
            .py_0p5()
            .gap_1()
            .items_center()
            .rounded_full()
            .border_1()
            .border_color(if self.selected {
                accent.into()
            } else {
                pal.border
            })
            .text_caption()
            .text_color(if self.selected {
                pal.text_primary
            } else {
                pal.text_muted
            })
            .bg(if self.selected {
                theme::accent_tint()
            } else {
                pal.control
            })
            .hover(move |style| {
                style.bg(if self.selected {
                    theme::accent_tint_hover()
                } else {
                    pal.control_hover
                })
            })
            .focus_visible(move |style| {
                style
                    .bg(if self.selected {
                        theme::accent_tint_hover()
                    } else {
                        pal.control_hover
                    })
                    .border_color(accent)
            })
            .when_some(self.icon, |tab, icon| tab.child(icon.size_3()))
            .child(label.clone())
            .children(self.children)
            .when_some(self.on_click, |tab, handler| {
                tab.on_click(move |event, window, cx| handler(event, window, cx))
            })
            .when_some(self.delete, |tab, (id, handler)| {
                tab.child(
                    BaseButton::new(id)
                        .accessibility_label(tr!("Remove %{name}", name => label.to_string()))
                        .px_0p5()
                        .rounded_full()
                        .text_color(pal.text_muted)
                        .hover(|style| style.text_color(pal.text_primary))
                        .focus_visible(|style| style.text_color(pal.text_primary))
                        .child(Icon::new(IconName::Close).size_3())
                        .on_click(move |event, window, cx| {
                            cx.stop_propagation();
                            handler(event, window, cx);
                        }),
                )
            })
    }
}

/// A selected-state container for one DPI preset and its actions.
#[derive(IntoElement)]
pub(crate) struct PresetChip {
    base: Stateful<gpui::Div>,
    selected: bool,
    children: Vec<AnyElement>,
}

impl PresetChip {
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: h_flex().id(id),
            selected: false,
            children: Vec::new(),
        }
    }
}

impl Selectable for PresetChip {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl ParentElement for PresetChip {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for PresetChip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        let selected = self.selected;
        self.base
            .h(px(28.))
            .px_2()
            .gap_2()
            .items_center()
            .rounded(pal.control_radius)
            .selected_border(selected, pal)
            .bg(pal.control)
            .selected_fill(selected)
            .hover(move |style| {
                style.bg(if selected {
                    theme::accent_tint_hover()
                } else {
                    pal.control_hover
                })
            })
            .children(self.children)
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui::{
        AppContext as _, Context, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, Render,
        TestAppContext, point,
    };

    use super::*;

    struct ToggleHarness {
        selected: bool,
        disabled: bool,
        changes: Rc<Cell<Option<bool>>>,
        parent_clicks: Rc<Cell<usize>>,
    }

    impl Render for ToggleHarness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let changes = self.changes.clone();
            let parent_clicks = self.parent_clicks.clone();
            div()
                .id("toggle-parent")
                .tab_group()
                .size(px(100.))
                .on_click(move |_, _, _| parent_clicks.set(parent_clicks.get() + 1))
                .child(
                    Toggle::new("keyboard-toggle")
                        .selected(self.selected)
                        .disabled(self.disabled)
                        .on_change(move |selected, _, _| changes.set(Some(*selected))),
                )
        }
    }

    struct MenuRowHarness {
        activations: Rc<Cell<usize>>,
    }

    impl Render for MenuRowHarness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let activations = self.activations.clone();
            div().tab_group().size(px(100.)).child(
                MenuRow::new("keyboard-menu-row")
                    .role(Role::MenuItem)
                    .child("Action")
                    .on_click(move |_, _, _| activations.set(activations.get() + 1)),
            )
        }
    }

    struct PresetChipHarness {
        applications: Rc<Cell<usize>>,
        removals: Rc<Cell<usize>>,
    }

    impl Render for PresetChipHarness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let applications = self.applications.clone();
            let removals = self.removals.clone();
            div().tab_group().size(px(100.)).child(
                PresetChip::new("keyboard-preset-chip")
                    .selected(true)
                    .child(
                        BaseButton::new("keyboard-preset-apply")
                            .child("800")
                            .on_click(move |_, _, _| {
                                applications.set(applications.get() + 1);
                            }),
                    )
                    .child(
                        BaseButton::new("keyboard-preset-remove")
                            .child(Icon::new(IconName::Close).size_3())
                            .on_click(move |_, _, _| removals.set(removals.get() + 1)),
                    ),
            )
        }
    }

    struct ProfileTabHarness {
        applications: Rc<Cell<usize>>,
        deletions: Rc<Cell<usize>>,
    }

    impl Render for ProfileTabHarness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let applications = self.applications.clone();
            let deletions = self.deletions.clone();
            div().tab_group().size(px(100.)).child(
                ProfileTab::new("keyboard-profile-tab", "Custom")
                    .on_click(move |_, _, _| applications.set(applications.get() + 1))
                    .on_delete("keyboard-profile-delete", move |_, _, _| {
                        deletions.set(deletions.get() + 1);
                    }),
            )
        }
    }

    fn activate_key(cx: &mut gpui::VisualTestContext, key: &str) {
        let keystroke = Keystroke::parse(key).unwrap();
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke });
    }

    #[gpui::test]
    fn toggle_is_tab_focusable_and_reports_controlled_next_state(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let changes = Rc::new(Cell::new(None));
        let parent_clicks = Rc::new(Cell::new(0));
        let (view, cx) = cx.add_window_view({
            let changes = changes.clone();
            let parent_clicks = parent_clicks.clone();
            move |_, _| ToggleHarness {
                selected: false,
                disabled: false,
                changes,
                parent_clicks,
            }
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        cx.update(Window::focus_next);
        cx.update(|window, cx| assert!(window.focused(cx).is_some()));

        activate_key(cx, "enter");
        assert_eq!(changes.get(), Some(true));

        view.update(cx, |view, cx| {
            view.selected = true;
            cx.notify();
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        activate_key(cx, "space");
        assert_eq!(changes.get(), Some(false));
    }

    #[gpui::test]
    fn disabled_toggle_is_inert_and_blocks_its_parent(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let changes = Rc::new(Cell::new(None));
        let parent_clicks = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let changes = changes.clone();
            let parent_clicks = parent_clicks.clone();
            move |_, _| ToggleHarness {
                selected: false,
                disabled: true,
                changes,
                parent_clicks,
            }
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });

        cx.update(Window::focus_next);
        cx.update(|window, cx| assert!(window.focused(cx).is_none()));
        cx.simulate_click(point(px(10.), px(10.)), Modifiers::default());
        activate_key(cx, "enter");
        activate_key(cx, "space");

        assert_eq!(changes.get(), None);
        assert_eq!(parent_clicks.get(), 0);
    }

    #[gpui::test]
    fn menu_row_is_tab_focusable_and_keyboard_activatable(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let activations = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let activations = activations.clone();
            move |_, _| MenuRowHarness { activations }
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        cx.update(Window::focus_next);
        cx.update(|window, cx| assert!(window.focused(cx).is_some()));

        activate_key(cx, "enter");
        activate_key(cx, "space");

        assert_eq!(activations.get(), 2);
    }

    /// The guard is the point: `set_placeholder` notifies unconditionally, so
    /// an unguarded per-render restamp would re-render forever. Same text must
    /// not notify; a changed text must land.
    #[gpui::test]
    fn localize_placeholder_restamps_only_on_change(cx: &mut TestAppContext) {
        struct Blank;
        impl Render for Blank {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                gpui::Empty
            }
        }

        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|_, _| Blank);
        let input =
            cx.update(|window, cx| cx.new(|cx| InputState::new(window, cx).placeholder("old")));
        let notifies = Rc::new(Cell::new(0_usize));
        let _obs = cx.update(|_, cx| {
            cx.observe(&input, {
                let notifies = notifies.clone();
                move |_, _| notifies.set(notifies.get() + 1)
            })
        });

        cx.update(|window, cx| localize_placeholder(&input, "old".into(), window, cx));
        assert_eq!(notifies.get(), 0, "unchanged text must not notify");

        cx.update(|window, cx| localize_placeholder(&input, "new".into(), window, cx));
        assert_eq!(notifies.get(), 1);
        cx.update(|_, cx| {
            assert_eq!(*input.read(cx).presentation().placeholder(), "new");
        });
    }

    #[gpui::test]
    fn profile_tab_and_delete_are_separate_keyboard_targets(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let applications = Rc::new(Cell::new(0));
        let deletions = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let applications = applications.clone();
            let deletions = deletions.clone();
            move |_, _| ProfileTabHarness {
                applications,
                deletions,
            }
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });

        cx.update(Window::focus_next);
        activate_key(cx, "enter");
        activate_key(cx, "space");
        assert_eq!(applications.get(), 2);
        assert_eq!(deletions.get(), 0);

        cx.update(Window::focus_next);
        activate_key(cx, "enter");
        activate_key(cx, "space");
        assert_eq!(applications.get(), 2);
        assert_eq!(deletions.get(), 2);
    }

    #[gpui::test]
    fn preset_chip_children_are_separate_keyboard_targets(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let applications = Rc::new(Cell::new(0));
        let removals = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let applications = applications.clone();
            let removals = removals.clone();
            move |_, _| PresetChipHarness {
                applications,
                removals,
            }
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });

        cx.update(Window::focus_next);
        activate_key(cx, "enter");
        activate_key(cx, "space");
        assert_eq!(applications.get(), 2);
        assert_eq!(removals.get(), 0);

        cx.update(Window::focus_next);
        activate_key(cx, "enter");
        activate_key(cx, "space");
        assert_eq!(applications.get(), 2);
        assert_eq!(removals.get(), 2);
    }
}
