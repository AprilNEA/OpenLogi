//! Root view: header (device carousel placeholder), body (mouse model area
//! and configuration panel), footer (settings / version).
//!
//! The body currently hosts the Phase 2 [`DpiPanel`] while the surrounding
//! layout (mouse model + multi-tab config panel) is being filled in across
//! the remaining UI.md phases.

use gpui::{
    AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement, Render, Styled,
    Window, div, px, rgb,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use optminus_core::device::DeviceInventory;

use crate::components::dpi_panel::DpiPanel;
use crate::state::AppState;
use crate::theme::{BG_DARK, BORDER, FOOTER_H, HEADER_H, TEXT_MUTED, TEXT_PRIMARY};

/// Application root view.
pub struct AppView {
    /// Inventory snapshot from the startup HID probe. Will feed the carousel
    /// in Phase 3; held here so the data survives the restructuring.
    inventories: Vec<DeviceInventory>,
    dpi_panel: Entity<DpiPanel>,
}

impl AppView {
    pub fn new(inventories: Vec<DeviceInventory>, cx: &mut Context<Self>) -> Self {
        // The DPI panel reads its initial value from AppState, so seed the
        // global before spawning it.
        if !cx.has_global::<AppState>() {
            cx.set_global(AppState::new());
        }
        let dpi_panel = cx.new(DpiPanel::new);
        Self {
            inventories,
            dpi_panel,
        }
    }
}

impl Render for AppView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(rgb(BG_DARK))
            .text_color(rgb(TEXT_PRIMARY))
            .child(header(self.inventories.len()))
            .child(body(&self.dpi_panel))
            .child(footer(cx))
    }
}

fn header(device_count: usize) -> impl IntoElement {
    // Placeholder strip — Phase 3 will replace this with the carousel proper.
    h_flex()
        .h(px(HEADER_H))
        .w_full()
        .px_5()
        .gap_3()
        .items_center()
        .border_b_1()
        .border_color(rgb(BORDER))
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Options−"),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(TEXT_MUTED))
                .child(format!("{device_count} receivers")),
        )
}

fn body(dpi_panel: &Entity<DpiPanel>) -> impl IntoElement {
    h_flex()
        .flex_1()
        .w_full()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap_8()
        .p_8()
        .child(dpi_panel.clone())
}

fn footer(cx: &Context<AppView>) -> impl IntoElement {
    let theme = cx.theme();
    h_flex()
        .h(px(FOOTER_H))
        .w_full()
        .px_5()
        .gap_4()
        .items_center()
        .justify_between()
        .border_t_1()
        .border_color(rgb(BORDER))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("Settings · About"),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(concat!("v", env!("CARGO_PKG_VERSION"))),
        )
}
