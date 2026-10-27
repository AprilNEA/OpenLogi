//! Theme-aware battery visuals and readouts.
//!
//! The device gallery, detail header, and device-information panel all use
//! this component so charge thresholds, status labels, and the cold-start
//! charging state stay visually consistent.

use gpui::{
    App, BorderStyle, Bounds, Corners, FontWeight, Hsla, IntoElement, ParentElement, PathBuilder,
    RenderOnce, Styled, Window, canvas, point, prelude::FluentBuilder as _, px, quad, rgb, size,
};
use gpui_component::h_flex;
use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus};

use super::theme::{self, Palette, Typography as _};

/// What a battery reading can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChargeReading {
    /// A measured state of charge.
    Percent(u8),
    /// Only the firmware's coarse level: the device has no state-of-charge
    /// gauge, so there is no percentage to show.
    Level(BatteryLevel),
    /// Neither a percentage nor a level yet.
    NoReading,
}

fn charge_reading(battery: &BatteryInfo) -> ChargeReading {
    match battery.percentage {
        // Some devices cannot gauge charge while under load. On a cold start
        // they report 0% until a later poll, which must not be presented as
        // an empty or critically depleted battery.
        Some(0) if is_charging(battery.status) => ChargeReading::NoReading,
        Some(percentage) => ChargeReading::Percent(percentage),
        None if battery.level == BatteryLevel::Unknown => ChargeReading::NoReading,
        None => ChargeReading::Level(battery.level),
    }
}

/// True when a charging device has supplied neither a usable percentage nor
/// a level yet, so "Charging" is all there is to say.
pub(crate) fn battery_charging_no_reading(battery: &BatteryInfo) -> bool {
    is_charging(battery.status) && charge_reading(battery) == ChargeReading::NoReading
}

/// A battery readout with presentations sized for each desktop context.
#[derive(IntoElement)]
pub(crate) struct BatteryIndicator {
    battery: BatteryInfo,
    presentation: Presentation,
}

#[derive(Clone, Copy)]
enum Presentation {
    Inline,
    Glance,
    Status { online: bool },
    Summary,
}

impl BatteryIndicator {
    /// A terse glyph and value for the device-detail title bar.
    pub(crate) fn inline(battery: &BatteryInfo) -> Self {
        Self {
            battery: battery.clone(),
            presentation: Presentation::Inline,
        }
    }

    /// The tersest readout — glyph and value only — for a card corner whose
    /// context rides a hover tip (see [`Self::status`]).
    pub(crate) fn glance(battery: &BatteryInfo) -> Self {
        Self {
            battery: battery.clone(),
            presentation: Presentation::Glance,
        }
    }

    /// A gallery-card readout with status and last-known context.
    pub(crate) fn status(battery: &BatteryInfo, online: bool) -> Self {
        Self {
            battery: battery.clone(),
            presentation: Presentation::Status { online },
        }
    }

    /// A larger readout for the device-information panel.
    pub(crate) fn summary(battery: &BatteryInfo) -> Self {
        Self {
            battery: battery.clone(),
            presentation: Presentation::Summary,
        }
    }
}

impl RenderOnce for BatteryIndicator {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        match self.presentation {
            Presentation::Inline => inline_readout(&self.battery, pal).into_any_element(),
            Presentation::Glance => glance_readout(&self.battery, pal).into_any_element(),
            Presentation::Status { online } => {
                status_readout(&self.battery, online, pal).into_any_element()
            }
            Presentation::Summary => summary_readout(&self.battery, pal).into_any_element(),
        }
    }
}

fn inline_readout(battery: &BatteryInfo, pal: Palette) -> impl IntoElement {
    h_flex()
        .items_center()
        .gap_2()
        .text_caption()
        .child(battery_glyph(battery, pal, GlyphSize::Compact, pal.page))
        .child(
            gpui::div()
                .font_weight(FontWeight::MEDIUM)
                .text_color(pal.text_primary)
                .child(value_label(battery)),
        )
}

fn glance_readout(battery: &BatteryInfo, pal: Palette) -> impl IntoElement {
    h_flex()
        .items_center()
        .gap_2()
        .text_caption()
        .child(battery_glyph(battery, pal, GlyphSize::Compact, pal.panel))
    // The number is hidden for now — the glyph's fill carries it, and
    // [`glance_hint`] repeats it on hover. Re-enable if the glyph alone
    // proves too coarse.
    // .child(
    //     gpui::div()
    //         .font_weight(FontWeight::MEDIUM)
    //         .text_color(pal.text_primary)
    //         .child(value_label(battery)),
    // )
}

fn status_readout(battery: &BatteryInfo, online: bool, pal: Palette) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .gap_2()
        .text_caption()
        .child(battery_glyph(battery, pal, GlyphSize::Compact, pal.panel))
        .child(
            gpui::div()
                .font_weight(FontWeight::MEDIUM)
                .text_color(pal.text_primary)
                .child(value_label(battery)),
        )
        .when_some(context_label(battery, online), |row, label| {
            row.child(gpui::div().text_color(pal.text_muted).child(label))
        })
}

fn summary_readout(battery: &BatteryInfo, pal: Palette) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .gap_4()
        .child(battery_glyph(battery, pal, GlyphSize::Summary, pal.panel))
        .child(
            gpui::div()
                .text_heading()
                .text_color(pal.text_primary)
                .child(value_label(battery)),
        )
        .child(
            gpui::div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(summary_label(battery)),
        )
}

/// The headline value: the percentage, or the level's word for a device
/// that reports no percentage.
pub(crate) fn value_label(battery: &BatteryInfo) -> String {
    match charge_reading(battery) {
        ChargeReading::Percent(percentage) => format!("{percentage}%"),
        ChargeReading::Level(BatteryLevel::Full) => tr!("device.full").to_string(),
        ChargeReading::Level(BatteryLevel::Good) => tr!("device.battery_good").to_string(),
        ChargeReading::Level(BatteryLevel::Low | BatteryLevel::Critical) => {
            tr!("device.low_battery").to_string()
        }
        ChargeReading::Level(BatteryLevel::Unknown) | ChargeReading::NoReading => {
            if is_charging(battery.status) {
                tr!("device.charging").to_string()
            } else {
                "—".to_owned()
            }
        }
    }
}

/// The status word shown beside [`value_label`], unless it would only
/// repeat it (a level-only "Full" device that reports a full status).
fn secondary_label(battery: &BatteryInfo) -> Option<gpui::SharedString> {
    let label = match battery.status {
        BatteryStatus::Charging | BatteryStatus::ChargingSlow => Some(tr!("device.charging")),
        BatteryStatus::Full => Some(tr!("device.full")),
        BatteryStatus::Error => Some(tr!("device.battery_error")),
        BatteryStatus::Discharging | BatteryStatus::Unknown => {
            battery.needs_attention().then(|| tr!("device.low_battery"))
        }
    };
    label.filter(|label| label.as_ref() != value_label(battery))
}

/// Everything a [`BatteryIndicator::glance`] does not show — the value plus
/// the status and last-known words — for its caller's hover tip.
pub(crate) fn glance_hint(battery: &BatteryInfo, online: bool) -> String {
    match context_label(battery, online) {
        Some(context) => format!("{} · {context}", value_label(battery)),
        None => value_label(battery),
    }
}

fn context_label(battery: &BatteryInfo, online: bool) -> Option<String> {
    match (!online, secondary_label(battery)) {
        (true, Some(status)) => Some(format!("{} · {status}", tr!("device.last_known_battery"))),
        (true, None) => Some(tr!("device.last_known_battery").to_string()),
        (false, Some(status)) => Some(status.to_string()),
        (false, None) => None,
    }
}

fn summary_label(battery: &BatteryInfo) -> gpui::SharedString {
    if battery_charging_no_reading(battery) {
        return tr!("device.battery");
    }
    let label = match battery.status {
        BatteryStatus::Charging | BatteryStatus::ChargingSlow => tr!("device.charging"),
        BatteryStatus::Full => tr!("device.full"),
        BatteryStatus::Error => tr!("device.battery_error"),
        BatteryStatus::Discharging | BatteryStatus::Unknown => {
            if battery.needs_attention() {
                tr!("device.low_battery")
            } else {
                tr!("device.battery")
            }
        }
    };
    // The caption names the reading; it must not repeat the headline.
    if label.as_ref() == value_label(battery) {
        tr!("device.battery")
    } else {
        label
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BatteryTone {
    Normal,
    Charging,
    Full,
    Low,
    Critical,
    Error,
}

fn battery_tone(battery: &BatteryInfo) -> BatteryTone {
    match battery.status {
        BatteryStatus::Charging | BatteryStatus::ChargingSlow => BatteryTone::Charging,
        BatteryStatus::Error => BatteryTone::Error,
        BatteryStatus::Full => BatteryTone::Full,
        BatteryStatus::Discharging | BatteryStatus::Unknown => {
            if battery.level == BatteryLevel::Critical {
                BatteryTone::Critical
            } else if battery.needs_attention() {
                BatteryTone::Low
            } else {
                BatteryTone::Normal
            }
        }
    }
}

fn tone_color(tone: BatteryTone, pal: Palette) -> Hsla {
    match tone {
        BatteryTone::Charging | BatteryTone::Full => rgb(theme::STATUS_CONNECTED).into(),
        BatteryTone::Low => rgb(theme::STATUS_CONNECTING).into(),
        BatteryTone::Critical | BatteryTone::Error => rgb(theme::STATUS_DISABLED).into(),
        BatteryTone::Normal => pal.text_primary,
    }
}

#[derive(Clone, Copy)]
enum GlyphSize {
    Compact,
    Summary,
}

impl GlyphSize {
    const fn dimensions(self) -> (f32, f32) {
        match self {
            Self::Compact => (27., 14.),
            Self::Summary => (46., 22.),
        }
    }
}

#[derive(Clone, Copy)]
enum GlyphMark {
    None,
    Charging,
    Error,
}

#[derive(Clone, Copy)]
struct GlyphPaint {
    percentage: Option<u8>,
    track: Hsla,
    fill: Hsla,
    outline: Hsla,
    mark: GlyphMark,
    mark_color: Hsla,
    mark_contrast: Hsla,
}

fn battery_glyph(
    battery: &BatteryInfo,
    pal: Palette,
    glyph_size: GlyphSize,
    background: Hsla,
) -> impl IntoElement {
    let tone = battery_tone(battery);
    let color = tone_color(tone, pal);
    let mark = match battery.status {
        BatteryStatus::Charging | BatteryStatus::ChargingSlow => GlyphMark::Charging,
        BatteryStatus::Error => GlyphMark::Error,
        BatteryStatus::Discharging | BatteryStatus::Full | BatteryStatus::Unknown => {
            GlyphMark::None
        }
    };
    let paint = GlyphPaint {
        percentage: glyph_fill(charge_reading(battery)),
        track: pal.muted,
        fill: color,
        outline: pal.text_muted.opacity(0.78),
        mark,
        mark_color: color,
        mark_contrast: background,
    };
    let (width, height) = glyph_size.dimensions();
    canvas(
        move |_, _, _| paint,
        move |bounds, paint, window, _| paint_battery(bounds, paint, window),
    )
    .flex_none()
    .w(px(width))
    .h(px(height))
}

fn paint_battery(bounds: Bounds<gpui::Pixels>, paint: GlyphPaint, window: &mut Window) {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    let large = height > 14.;
    let cap_width = if large { 3. } else { 2. };
    let body_height = height - 2.;
    let body_width = width - cap_width;
    let body_x = f32::from(bounds.origin.x);
    let body_y = f32::from(bounds.origin.y) + 1.;
    let body = Bounds {
        origin: point(px(body_x), px(body_y)),
        size: size(px(body_width), px(body_height)),
    };
    let terminal_height = body_height * 0.38;
    let terminal = Bounds {
        origin: point(
            px(body_x + body_width - 0.5),
            px(body_y + (body_height - terminal_height) / 2.),
        ),
        size: size(px(cap_width), px(terminal_height)),
    };
    window.paint_quad(quad(
        terminal,
        px(cap_width / 2.),
        paint.outline,
        px(0.),
        paint.outline,
        BorderStyle::default(),
    ));

    let radius = px(if large { 4. } else { 3. });
    window.paint_quad(quad(
        body,
        radius,
        paint.track,
        px(1.),
        paint.outline,
        BorderStyle::default(),
    ));

    if let Some(percentage) = paint.percentage.filter(|percentage| *percentage > 0) {
        let inset = 2.;
        let inner_width = body_width - inset * 2.;
        let inner_height = body_height - inset * 2.;
        let fill_width = (inner_width * f32::from(percentage.min(100)) / 100.).max(1.);
        let fill_bounds = Bounds {
            origin: point(px(body_x + inset), px(body_y + inset)),
            size: size(px(fill_width), px(inner_height)),
        };
        let fill_radius: f32 = if large { 2.5 } else { 1.5 };
        let left_radius = px(fill_radius.min(fill_width / 2.));
        let right_radius = px(if percentage >= 96 { fill_radius } else { 0.5 });
        window.paint_quad(quad(
            fill_bounds,
            Corners {
                top_left: left_radius,
                top_right: right_radius,
                bottom_right: right_radius,
                bottom_left: left_radius,
            },
            paint.fill,
            px(0.),
            paint.fill,
            BorderStyle::default(),
        ));
    }

    match paint.mark {
        GlyphMark::None => {}
        GlyphMark::Charging => {
            paint_charging_mark(body, mark_color(paint), window);
        }
        GlyphMark::Error => {
            paint_error_mark(body, mark_color(paint), window);
        }
    }
}

fn mark_color(paint: GlyphPaint) -> Hsla {
    if paint.percentage.is_some_and(|percentage| percentage >= 55) {
        paint.mark_contrast
    } else {
        paint.mark_color
    }
}

fn paint_charging_mark(body: Bounds<gpui::Pixels>, color: Hsla, window: &mut Window) {
    let center_x = f32::from(body.origin.x) + f32::from(body.size.width) / 2.;
    let center_y = f32::from(body.origin.y) + f32::from(body.size.height) / 2.;
    let mark_height = f32::from(body.size.height) * 0.76;
    let mark_width = mark_height * 0.54;
    let mut path = PathBuilder::fill();
    path.add_polygon(
        &[
            point(
                px(center_x + mark_width * 0.12),
                px(center_y - mark_height / 2.),
            ),
            point(
                px(center_x - mark_width * 0.42),
                px(center_y + mark_height * 0.05),
            ),
            point(
                px(center_x - mark_width * 0.06),
                px(center_y + mark_height * 0.05),
            ),
            point(
                px(center_x - mark_width * 0.18),
                px(center_y + mark_height / 2.),
            ),
            point(
                px(center_x + mark_width * 0.45),
                px(center_y - mark_height * 0.12),
            ),
            point(
                px(center_x + mark_width * 0.08),
                px(center_y - mark_height * 0.12),
            ),
        ],
        true,
    );
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn paint_error_mark(body: Bounds<gpui::Pixels>, color: Hsla, window: &mut Window) {
    let center_x = f32::from(body.origin.x) + f32::from(body.size.width) / 2.;
    let center_y = f32::from(body.origin.y) + f32::from(body.size.height) / 2.;
    let height = f32::from(body.size.height);
    let stroke = (height * 0.13).max(1.);
    let line_height = height * 0.34;
    window.paint_quad(quad(
        Bounds {
            origin: point(px(center_x - stroke / 2.), px(center_y - height * 0.32)),
            size: size(px(stroke), px(line_height)),
        },
        px(stroke / 2.),
        color,
        px(0.),
        color,
        BorderStyle::default(),
    ));
    window.paint_quad(quad(
        Bounds {
            origin: point(px(center_x - stroke / 2.), px(center_y + height * 0.2)),
            size: size(px(stroke), px(stroke)),
        },
        px(stroke / 2.),
        color,
        px(0.),
        color,
        BorderStyle::default(),
    ));
}

/// How full to draw the glyph. A level-only device fills to a fixed point
/// inside its level's band, so the glyph still reads at a glance.
const fn glyph_fill(reading: ChargeReading) -> Option<u8> {
    match reading {
        ChargeReading::Percent(percentage) => Some(percentage),
        ChargeReading::Level(BatteryLevel::Critical) => Some(10),
        ChargeReading::Level(BatteryLevel::Low) => Some(25),
        ChargeReading::Level(BatteryLevel::Good) => Some(70),
        ChargeReading::Level(BatteryLevel::Full) => Some(100),
        ChargeReading::Level(BatteryLevel::Unknown) | ChargeReading::NoReading => None,
    }
}

const fn is_charging(status: BatteryStatus) -> bool {
    matches!(
        status,
        BatteryStatus::Charging | BatteryStatus::ChargingSlow
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_respects_status_priority_and_discharge_thresholds() {
        let cases = [
            (
                5,
                BatteryLevel::Critical,
                BatteryStatus::Charging,
                BatteryTone::Charging,
            ),
            (
                80,
                BatteryLevel::Good,
                BatteryStatus::Error,
                BatteryTone::Error,
            ),
            (
                5,
                BatteryLevel::Critical,
                BatteryStatus::Full,
                BatteryTone::Full,
            ),
            (
                8,
                BatteryLevel::Critical,
                BatteryStatus::Discharging,
                BatteryTone::Critical,
            ),
            (
                20,
                BatteryLevel::Low,
                BatteryStatus::Discharging,
                BatteryTone::Low,
            ),
            (
                21,
                BatteryLevel::Low,
                BatteryStatus::Discharging,
                BatteryTone::Normal,
            ),
        ];
        for (percentage, level, status, expected) in cases {
            let battery = BatteryInfo {
                percentage: Some(percentage),
                level,
                status,
            };
            assert_eq!(battery_tone(&battery), expected);
        }
    }

    fn level_only(level: BatteryLevel, status: BatteryStatus) -> BatteryInfo {
        BatteryInfo {
            percentage: None,
            level,
            status,
        }
    }

    #[test]
    fn a_reading_without_a_percentage_falls_back_to_the_level() {
        let cases = [
            (
                Some(0),
                BatteryLevel::Good,
                BatteryStatus::Charging,
                ChargeReading::NoReading,
            ),
            (
                Some(0),
                BatteryLevel::Critical,
                BatteryStatus::Discharging,
                ChargeReading::Percent(0),
            ),
            (
                Some(64),
                BatteryLevel::Good,
                BatteryStatus::Discharging,
                ChargeReading::Percent(64),
            ),
            (
                None,
                BatteryLevel::Good,
                BatteryStatus::Discharging,
                ChargeReading::Level(BatteryLevel::Good),
            ),
            (
                None,
                BatteryLevel::Low,
                BatteryStatus::Charging,
                ChargeReading::Level(BatteryLevel::Low),
            ),
            (
                None,
                BatteryLevel::Unknown,
                BatteryStatus::Charging,
                ChargeReading::NoReading,
            ),
            (
                None,
                BatteryLevel::Unknown,
                BatteryStatus::Unknown,
                ChargeReading::NoReading,
            ),
        ];
        for (percentage, level, status, expected) in cases {
            let battery = BatteryInfo {
                percentage,
                level,
                status,
            };
            assert_eq!(charge_reading(&battery), expected, "{battery:?}");
        }
    }

    #[test]
    fn a_level_only_battery_is_toned_by_its_level() {
        assert_eq!(
            battery_tone(&level_only(BatteryLevel::Critical, BatteryStatus::Charging)),
            BatteryTone::Charging
        );
        assert_eq!(
            battery_tone(&level_only(
                BatteryLevel::Critical,
                BatteryStatus::Discharging
            )),
            BatteryTone::Critical
        );
        assert_eq!(
            battery_tone(&level_only(BatteryLevel::Low, BatteryStatus::Discharging)),
            BatteryTone::Low
        );
        assert_eq!(
            battery_tone(&level_only(BatteryLevel::Good, BatteryStatus::Discharging)),
            BatteryTone::Normal
        );
    }

    #[test]
    fn a_level_only_glyph_fills_inside_its_level_band() {
        assert_eq!(glyph_fill(ChargeReading::Percent(42)), Some(42));
        assert_eq!(
            glyph_fill(ChargeReading::Level(BatteryLevel::Critical)),
            Some(10)
        );
        assert_eq!(
            glyph_fill(ChargeReading::Level(BatteryLevel::Low)),
            Some(25)
        );
        assert_eq!(
            glyph_fill(ChargeReading::Level(BatteryLevel::Good)),
            Some(70)
        );
        assert_eq!(
            glyph_fill(ChargeReading::Level(BatteryLevel::Full)),
            Some(100)
        );
        assert_eq!(glyph_fill(ChargeReading::NoReading), None);
    }
}
