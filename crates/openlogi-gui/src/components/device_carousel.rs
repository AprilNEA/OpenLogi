use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, BorrowAppContext as _, BoxShadow, Context, Entity,
    FontWeight, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement as _, Styled, Window, div, ease_in_out, point,
    prelude::FluentBuilder as _, pulsating_between, px, rgb,
};
use gpui_component::{h_flex, v_flex};
use openlogi_core::device::{
    BatteryInfo, BatteryStatus, DeviceInventory, DeviceKind, PairedDevice,
};

use crate::state::AppState;
use crate::theme::{
    self, ACCENT_BLUE, Palette, STATUS_CONNECTED, STATUS_CONNECTING, STATUS_OFFLINE,
};

const CARD_W: f32 = 220.;
const CARD_H: f32 = 64.;
const DOT_SIZE: f32 = 10.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Connected,
    Connecting,
    Offline,
}

impl Status {
    fn color(self) -> u32 {
        match self {
            Status::Connected => STATUS_CONNECTED,
            Status::Connecting => STATUS_CONNECTING,
            Status::Offline => STATUS_OFFLINE,
        }
    }
}

#[derive(Clone)]
struct CardData {
    name: String,
    sub: String,
    status: Status,
    battery: Option<BatteryInfo>,
}

/// Header carousel for paired devices.
pub struct DeviceCarousel {
    cards: Vec<CardData>,
}

impl DeviceCarousel {
    /// Build device cards from the current inventories.
    pub fn new(inventories: &[DeviceInventory], _cx: &mut Context<Self>) -> Self {
        let mut cards: Vec<CardData> = inventories
            .iter()
            .flat_map(|inv| inv.paired.iter().map(card_from_paired))
            .collect();

        if cards.is_empty() {
            cards = demo_cards();
        }

        Self { cards }
    }
}

impl Render for DeviceCarousel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = cx
            .try_global::<AppState>()
            .map_or(0, |s| s.current_device)
            .min(self.cards.len().saturating_sub(1));
        let entity = cx.entity();
        let pal = theme::palette(cx);

        h_flex()
            .id("device-carousel")
            .gap_3()
            .items_center()
            .overflow_x_scroll()
            .children(
                self.cards
                    .iter()
                    .enumerate()
                    .map(|(idx, card)| card_view(idx, card, idx == selected, &entity, pal)),
            )
    }
}

fn card_view(
    idx: usize,
    card: &CardData,
    selected: bool,
    entity: &Entity<DeviceCarousel>,
    pal: Palette,
) -> AnyElement {
    let battery_label = card.battery.as_ref().map(format_battery);
    let entity = entity.clone();

    div()
        .id(("device-card", idx))
        .w(px(CARD_W))
        .h(px(CARD_H))
        .px_3()
        .py_2()
        .rounded_md()
        .border_2()
        .border_color(if selected {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.border
        })
        .bg(pal.surface)
        .hover(|s| s.bg(pal.surface_hover))
        .on_click(move |_event, _window, cx| {
            cx.update_global::<AppState, _>(|state, _| state.set_current_device(idx));
            entity.update(cx, |_, cx| cx.notify());
        })
        .child(
            h_flex()
                .size_full()
                .gap_3()
                .items_center()
                .child(status_dot(card.status))
                .child(
                    v_flex()
                        .gap_0p5()
                        .flex_1()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(card.name.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(pal.text_muted)
                                .child(card.sub.clone()),
                        ),
                )
                .when_some(battery_label, |this, label| {
                    this.child(div().text_xs().text_color(pal.text_muted).child(label))
                }),
        )
        .into_any_element()
}

fn status_dot(status: Status) -> AnyElement {
    let base = div()
        .size(px(DOT_SIZE))
        .rounded_full()
        .bg(rgb(status.color()));
    match status {
        Status::Offline => base.into_any_element(),
        Status::Connecting => base
            .with_animation(
                "status-fast",
                Animation::new(Duration::from_millis(450))
                    .repeat()
                    .with_easing(pulsating_between(0.3, 1.)),
                Styled::opacity,
            )
            .into_any_element(),
        Status::Connected => base
            .with_animation(
                "status-breath",
                Animation::new(Duration::from_millis(2200))
                    .repeat()
                    .with_easing(ease_in_out),
                |this, delta| {
                    let intensity = (delta * std::f32::consts::PI).sin();
                    this.shadow(vec![BoxShadow {
                        color: gpui::hsla(0.35, 0.7, 0.55, 0.35 + intensity * 0.45),
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(2. + intensity * 8.),
                        spread_radius: px(0.5),
                    }])
                },
            )
            .into_any_element(),
    }
}

fn card_from_paired(d: &PairedDevice) -> CardData {
    let name = d
        .codename
        .clone()
        .unwrap_or_else(|| format!("Slot {}", d.slot));
    let sub = format!("{} · slot {}", kind_label(d.kind), d.slot);
    let status = if d.online {
        Status::Connected
    } else {
        Status::Offline
    };
    CardData {
        name,
        sub,
        status,
        battery: d.battery.clone(),
    }
}

fn demo_cards() -> Vec<CardData> {
    vec![
        CardData {
            name: "MX Master".into(),
            sub: "Mouse · slot 1".into(),
            status: Status::Connected,
            battery: None,
        },
        CardData {
            name: "Lift".into(),
            sub: "Mouse · slot 2".into(),
            status: Status::Connecting,
            battery: None,
        },
        CardData {
            name: "M650".into(),
            sub: "Mouse · slot 3".into(),
            status: Status::Offline,
            battery: None,
        },
    ]
}

fn format_battery(b: &BatteryInfo) -> String {
    let glyph = match b.status {
        BatteryStatus::Charging | BatteryStatus::ChargingSlow => "⚡ ",
        BatteryStatus::Full => "✓ ",
        BatteryStatus::Error => "⚠ ",
        _ => "",
    };
    format!("{glyph}{}%", b.percentage)
}

fn kind_label(kind: DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Mouse => "Mouse",
        DeviceKind::Keyboard => "Keyboard",
        DeviceKind::Numpad => "Numpad",
        DeviceKind::Presenter => "Presenter",
        DeviceKind::Remote => "Remote",
        DeviceKind::Trackball => "Trackball",
        DeviceKind::Touchpad => "Touchpad",
        DeviceKind::Tablet => "Tablet",
        DeviceKind::Gamepad => "Gamepad",
        DeviceKind::Joystick => "Joystick",
        DeviceKind::Headset => "Headset",
        DeviceKind::Unknown => "Device",
    }
}
