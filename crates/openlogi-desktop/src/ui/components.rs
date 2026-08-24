//! Reusable, theme-aware desktop components.
//!
//! These components own their semantic state and resolve the active palette at
//! render time, so callers do not have to thread colours through free-function
//! builders.

use gpui::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement, Interactivity, IntoElement,
    ParentElement, Pixels, RenderOnce, SharedString, Stateful, StatefulInteractiveElement, Styled,
    Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::{
    Disableable, Icon, IconName, Selectable, Sizable, Size, h_flex, switch::Switch, v_flex,
};

use super::theme::{self, ACCENT_BLUE, SelectableStyle as _, Typography as _};

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
            .p(px(theme::CARD_PAD))
            .child(
                v_flex()
                    .gap(px(theme::CARD_GAP))
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

/// A full-width row in an action menu.
#[derive(IntoElement)]
pub(crate) struct MenuRow {
    base: Stateful<gpui::Div>,
    selected: bool,
    children: Vec<AnyElement>,
}

impl MenuRow {
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: h_flex().id(id),
            selected: false,
            children: Vec::new(),
        }
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
            .w_full()
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
            .children(self.children)
    }
}

/// A selectable camera-profile chip.
#[derive(IntoElement)]
pub(crate) struct ProfileTab {
    base: Stateful<gpui::Div>,
    label: SharedString,
    selected: bool,
    children: Vec<AnyElement>,
    delete: Option<(ElementId, ClickHandler)>,
}

type ClickHandler = std::rc::Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

impl ProfileTab {
    pub(crate) fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            base: h_flex().id(id),
            label: label.into(),
            selected: false,
            children: Vec::new(),
            delete: None,
        }
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
        self.base
            .when(!has_delete, gpui::Styled::px_2)
            .when(has_delete, |tab| tab.pl_2().pr_1())
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
            .child(self.label)
            .children(self.children)
            .when_some(self.delete, |tab, (id, handler)| {
                tab.child(
                    div()
                        .id(id)
                        .px_0p5()
                        .rounded_full()
                        .text_color(pal.text_muted)
                        .hover(|style| style.text_color(pal.text_primary))
                        .child("×")
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
