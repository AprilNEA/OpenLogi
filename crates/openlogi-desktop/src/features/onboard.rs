//! The settings stored in a gaming mouse's onboard memory (HID++
//! `0x8100`), edited on the cards whose live settings they replace: the
//! Pointer card edits the DPI stages its DPI button cycles and the report
//! rate, the Lighting card the effect of each LED zone. Each card's
//! onboard-memory toggle chooses between this editor and OpenLogi's live
//! controls. An edit is a draft, saved to the mouse in one write.
//!
//! The draft follows the mouse while it is untouched: a newer reading — the
//! read-back of a save included — replaces it. Once edited it is kept until
//! it is saved or reverted.

use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div, prelude::FluentBuilder as _, rems,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{InputEvent, InputState, NumberInput},
    radio::Radio,
    v_flex,
};
use openlogi_core::color::Rgb;
use openlogi_core::hid::{
    Dpi, DpiCapabilities, OnboardLighting, OnboardMemory, OnboardProfile, OnboardProfileChange,
    ReportRate, WriteError,
};

use crate::features::lighting::keyboard_rgb::{PALETTE, color_swatch};
use crate::state::{AppState, DeviceKey, DpiLoad, OnboardLoad, ReportRateLoad, StateEvent};
use crate::ui::components::{control_button, localize_placeholder};
use crate::ui::section::section_label;
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, Typography as _};

/// How many DPI stages a profile holds.
const STAGES: usize = 5;

/// Report rates a profile can store when the mouse has not listed its own.
const FALLBACK_RATES: [u16; 4] = [125, 250, 500, 1000];

/// What an effect starts with when the user switches a zone to it.
const BREATHING_PERIOD_MS: u16 = 4000;
const CYCLE_PERIOD_MS: u16 = 10_000;

/// The part of the onboard memory a panel shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnboardSection {
    /// The stored DPI stages and report rate.
    Pointer,
    /// The stored effect of each LED zone.
    Lighting,
}

pub struct OnboardPanel {
    section: OnboardSection,
    /// One text field per DPI stage; empty means the stage is skipped.
    stages: Vec<Entity<InputState>>,
    draft: Option<Draft>,
    /// Why the last save was not sent, until the draft changes.
    invalid: Option<SharedString>,
    _state_obs: Subscription,
    _stage_subs: Vec<Subscription>,
}

/// The edits not yet saved to the mouse, other than the DPI fields.
#[derive(Clone, PartialEq)]
struct Draft {
    key: DeviceKey,
    /// The reading the draft started from.
    base: OnboardProfile,
    default_slot: u8,
    report_rate: Option<ReportRate>,
    lighting: Vec<OnboardLighting>,
}

impl Draft {
    fn of(key: DeviceKey, profile: &OnboardProfile) -> Self {
        Self {
            key,
            base: profile.clone(),
            default_slot: profile.default_dpi_slot,
            report_rate: profile.report_rate,
            lighting: profile.lighting.clone(),
        }
    }

    /// Whether the draft holds what `profile` holds in `section`, DPI
    /// fields aside.
    fn matches(&self, section: OnboardSection, profile: &OnboardProfile) -> bool {
        match section {
            OnboardSection::Pointer => {
                self.default_slot == profile.default_dpi_slot
                    && self.report_rate == profile.report_rate
            }
            OnboardSection::Lighting => self.lighting == profile.lighting,
        }
    }
}

/// The text a DPI field shows for a stage.
fn stage_text(dpi: Option<Dpi>) -> String {
    dpi.map(|dpi| dpi.to_string()).unwrap_or_default()
}

/// What the panel needs about the selected device.
struct Facts {
    key: DeviceKey,
    reachable: bool,
    load: OnboardLoad,
    writing: bool,
    refusal: Option<WriteError>,
    rates: Vec<ReportRate>,
    sensor: Option<DpiCapabilities>,
}

impl Facts {
    fn current(cx: &App) -> Option<Self> {
        let state = AppState::try_read(cx)?;
        if !state.current_onboard_supported() {
            return None;
        }
        let record = state.current_record()?;
        let key = record.device_key();
        let rates = match state.current_report_rate_load() {
            // The profile stores intervals of 1–8 ms, so a live rate above
            // 1 kHz (2, 4 or 8 kHz) has no stored form and would always fail
            // to save.
            ReportRateLoad::Ready(info) => info
                .supported
                .iter()
                .copied()
                .filter(|rate| rate.interval_ms().is_some())
                .collect(),
            _ => FALLBACK_RATES.into_iter().map(ReportRate::new).collect(),
        };
        let sensor = match state.dpi_load_for(&key) {
            DpiLoad::Ready(info) => Some(info.capabilities.clone()),
            _ => None,
        };
        Some(Self {
            reachable: record.route.is_some() && record.online,
            load: state.current_onboard_load(),
            writing: state.current_onboard_writing(),
            refusal: state.current_onboard_refusal(),
            rates,
            sensor,
            key,
        })
    }
}

impl OnboardPanel {
    pub fn new(section: OnboardSection, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state_obs = AppState::repaint_on(cx, |event| {
            matches!(
                event,
                StateEvent::OnboardChanged(_)
                    | StateEvent::DpiChanged(_)
                    | StateEvent::ReportRateChanged(_)
                    | StateEvent::DeviceConfigChanged(_)
                    | StateEvent::LightingChanged(_)
            )
        });
        let mut stage_subs = Vec::with_capacity(STAGES);
        let fields = if section == OnboardSection::Pointer {
            STAGES
        } else {
            0
        };
        let stages = (0..fields)
            .map(|_| {
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .validate(|text, _| {
                            text.len() <= 5 && text.chars().all(|c| c.is_ascii_digit())
                        })
                        .step_by(|_, _, _| 50.)
                });
                stage_subs.push(cx.subscribe(&input, |panel, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        panel.invalid = None;
                        cx.notify();
                    }
                }));
                input
            })
            .collect();
        Self {
            section,
            stages,
            draft: None,
            invalid: None,
            _state_obs: state_obs,
            _stage_subs: stage_subs,
        }
    }

    /// The DPI fields as they read now: `None` for an empty field, `Err` for
    /// one that is not a number.
    fn stage_values(&self, cx: &App) -> Vec<Result<Option<Dpi>, ()>> {
        self.stages
            .iter()
            .map(|input| {
                let text = input.read(cx).value();
                let text = text.trim();
                if text.is_empty() {
                    Ok(None)
                } else {
                    text.parse::<u16>()
                        .map(|dpi| Some(Dpi::new(dpi)))
                        .map_err(|_| ())
                }
            })
            .collect()
    }

    /// Whether the user changed anything since the draft's reading.
    fn dirty(&self, cx: &App) -> bool {
        let Some(draft) = &self.draft else {
            return false;
        };
        !draft.matches(self.section, &draft.base)
            || self
                .stage_values(cx)
                .iter()
                .zip(&draft.base.dpi_slots)
                .any(|(field, stored)| field.as_ref() != Ok(stored))
    }

    /// Follow `profile` unless the user has edits it does not hold.
    fn sync(
        &mut self,
        key: &DeviceKey,
        profile: &OnboardProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let follows = match &self.draft {
            None => true,
            Some(draft) if draft.key != *key => true,
            Some(draft) if draft.base == *profile => return,
            Some(draft) => !self.dirty(cx) || self.holds(draft, profile, cx),
        };
        if follows {
            self.reset(key, profile, window, cx);
        }
    }

    /// Whether the draft and fields hold exactly `profile` — a save's
    /// read-back.
    fn holds(&self, draft: &Draft, profile: &OnboardProfile, cx: &App) -> bool {
        draft.matches(self.section, profile)
            && self
                .stage_values(cx)
                .iter()
                .zip(&profile.dpi_slots)
                .all(|(field, stored)| field.as_ref() == Ok(stored))
    }

    fn reset(
        &mut self,
        key: &DeviceKey,
        profile: &OnboardProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for (input, dpi) in self.stages.iter().zip(
            profile
                .dpi_slots
                .iter()
                .copied()
                .chain(std::iter::repeat(None)),
        ) {
            input.update(cx, |input, cx| input.set_value(stage_text(dpi), window, cx));
        }
        self.draft = Some(Draft::of(key.clone(), profile));
        self.invalid = None;
    }

    /// The change to save, or why it cannot be saved. `latest` is the newest
    /// reading: the fields this card did not edit come from it, so an edit
    /// left in the other card's draft cannot undo a save made there since.
    fn change(
        &self,
        latest: &OnboardProfile,
        sensor: Option<&DpiCapabilities>,
        cx: &App,
    ) -> Result<OnboardProfileChange, SharedString> {
        let draft = self.draft.as_ref().ok_or_else(SharedString::default)?;
        let base = &draft.base;
        let mut dpi_slots = latest.dpi_slots.clone();
        if self.section == OnboardSection::Pointer {
            for (stage, field) in self.stage_values(cx).into_iter().enumerate() {
                let dpi = field.map_err(|()| stage_error(stage, sensor))?;
                if let (Some(dpi), Some(sensor)) = (dpi, sensor)
                    && !sensor.contains(dpi)
                {
                    return Err(stage_error(stage, Some(sensor)));
                }
                if dpi != base.dpi_slots.get(stage).copied().flatten()
                    && let Some(slot) = dpi_slots.get_mut(stage)
                {
                    *slot = dpi;
                }
            }
        }
        let default_dpi_slot = if draft.default_slot == base.default_dpi_slot {
            latest.default_dpi_slot
        } else {
            draft.default_slot
        };
        if dpi_slots
            .get(usize::from(default_dpi_slot))
            .copied()
            .flatten()
            .is_none()
        {
            return Err(tr!("onboard.default_stage_empty"));
        }
        let report_rate = if draft.report_rate == base.report_rate {
            latest.report_rate.or(draft.report_rate)
        } else {
            draft.report_rate
        }
        .ok_or_else(|| tr!("onboard.choose_report_rate"))?;
        let lighting = if draft.lighting == base.lighting {
            latest.lighting.clone()
        } else {
            draft.lighting.clone()
        };
        Ok(OnboardProfileChange {
            dpi_slots,
            default_dpi_slot,
            report_rate,
            lighting,
        })
    }

    fn save(
        &mut self,
        latest: &OnboardProfile,
        sensor: Option<&DpiCapabilities>,
        cx: &mut Context<Self>,
    ) {
        match self.change(latest, sensor, cx) {
            Ok(change) => {
                self.invalid = None;
                AppState::apply(cx, |state| state.commit_onboard_profile(change));
            }
            Err(reason) => self.invalid = Some(reason),
        }
        cx.notify();
    }

    fn edit(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Draft)) {
        if let Some(draft) = &mut self.draft {
            edit(draft);
            self.invalid = None;
            cx.notify();
        }
    }
}

/// The error for DPI stage `stage` (zero-based), naming the sensor's range
/// when it is known.
fn stage_error(stage: usize, sensor: Option<&DpiCapabilities>) -> SharedString {
    let n = stage + 1;
    match sensor {
        Some(sensor) => tr!(
            "onboard.stage_out_of_range",
            n => n,
            min => sensor.min(),
            max => sensor.max(),
            step => sensor.step_hint()
        ),
        None => tr!("onboard.stage_not_a_number", n => n),
    }
}

impl Render for OnboardPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        for input in &self.stages {
            localize_placeholder(input, tr!("onboard.stage_off"), window, cx);
        }
        let Some(facts) = Facts::current(cx) else {
            self.draft = None;
            return div().into_any_element();
        };
        let memory: Option<Arc<OnboardMemory>> = match &facts.load {
            OnboardLoad::Ready(memory) => Some(memory.clone()),
            _ => None,
        };
        if let Some(profile) = memory.as_ref().and_then(|memory| memory.profile.as_ref()) {
            self.sync(&facts.key, profile, window, cx);
        } else {
            self.draft = None;
        }

        self.profile_body(&facts, memory.as_deref(), pal, cx)
    }
}

/// The effects a zone can be switched to, in the order they are offered.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Effect {
    Off,
    Color,
    Breathing,
    Cycle,
}

impl Effect {
    const ALL: [Self; 4] = [Self::Off, Self::Color, Self::Breathing, Self::Cycle];

    fn of(lighting: OnboardLighting) -> Option<Self> {
        match lighting {
            OnboardLighting::Off => Some(Self::Off),
            OnboardLighting::Fixed(_) => Some(Self::Color),
            OnboardLighting::Breathing { .. } => Some(Self::Breathing),
            OnboardLighting::Cycle { .. } => Some(Self::Cycle),
            OnboardLighting::Unsupported => None,
        }
    }

    fn label(self) -> SharedString {
        match self {
            Self::Off => tr!("onboard.effect_off"),
            Self::Color => tr!("onboard.effect_color"),
            Self::Breathing => tr!("onboard.effect_breathing"),
            Self::Cycle => tr!("onboard.effect_cycle"),
        }
    }

    /// `current` switched to this effect, keeping its color.
    fn apply(self, current: OnboardLighting) -> OnboardLighting {
        let color = match current {
            OnboardLighting::Fixed(color) | OnboardLighting::Breathing { color, .. } => color,
            _ => Rgb::WHITE,
        };
        match self {
            Self::Off => OnboardLighting::Off,
            Self::Color => OnboardLighting::Fixed(color),
            Self::Breathing => OnboardLighting::Breathing {
                color,
                period_ms: BREATHING_PERIOD_MS,
                brightness: 100,
            },
            Self::Cycle => OnboardLighting::Cycle {
                period_ms: CYCLE_PERIOD_MS,
                brightness: 100,
            },
        }
    }
}

/// `lighting` with its color replaced, for the effects that have one.
fn with_color(lighting: OnboardLighting, color: Rgb) -> OnboardLighting {
    match lighting {
        OnboardLighting::Fixed(_) => OnboardLighting::Fixed(color),
        OnboardLighting::Breathing {
            period_ms,
            brightness,
            ..
        } => OnboardLighting::Breathing {
            color,
            period_ms,
            brightness,
        },
        other => other,
    }
}

impl OnboardPanel {
    /// The stored-profile card's body for the reading in hand.
    fn profile_body(
        &self,
        facts: &Facts,
        memory: Option<&OnboardMemory>,
        pal: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !facts.reachable {
            return status_line(tr!("onboard.connect_to_read"), pal).into_any_element();
        }
        let Some(memory) = memory else {
            return match &facts.load {
                OnboardLoad::Failed(_) => {
                    let key = facts.key.clone();
                    retry_line(
                        "onboard-retry",
                        tr!("onboard.read_failed_click_to_retry"),
                        pal,
                        move |cx| AppState::apply(cx, |state| state.retry_onboard_read(&key)),
                    )
                    .into_any_element()
                }
                OnboardLoad::Unsupported(_) => {
                    status_line(tr!("onboard.layout_unsupported"), pal).into_any_element()
                }
                OnboardLoad::Unknown | OnboardLoad::Loading | OnboardLoad::Ready(_) => {
                    status_line(tr!("onboard.reading"), pal).into_any_element()
                }
            };
        };
        let (Some(profile), Some(draft)) = (memory.profile.as_ref(), self.draft.as_ref()) else {
            return status_line(tr!("onboard.layout_unsupported"), pal).into_any_element();
        };
        let dirty = self.dirty(cx);
        // The tab's edit-scope switch says what is edited and when it
        // applies; only a factory profile needs saying here.
        v_flex()
            .gap_5()
            .when(profile.factory, |this| {
                this.child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(tr!("onboard.factory_profile")),
                )
            })
            .when(self.section == OnboardSection::Pointer, |this| {
                this.child(self.stages_section(draft, facts.writing, pal, cx))
                    .child(Self::rate_section(
                        draft,
                        &facts.rates,
                        facts.writing,
                        pal,
                        cx,
                    ))
            })
            .when(self.section == OnboardSection::Lighting, |this| {
                this.child(Self::lighting_section(draft, facts.writing, pal, cx))
            })
            .child(self.footer(facts, profile, dirty, pal, cx))
            .into_any_element()
    }

    fn stages_section(
        &self,
        draft: &Draft,
        writing: bool,
        pal: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_2()
            .child(section_label(tr!("onboard.dpi_stages"), pal))
            .children(self.stages.iter().enumerate().map(|(stage, input)| {
                let is_default = usize::from(draft.default_slot) == stage;
                let slot = u8::try_from(stage).unwrap_or(u8::MAX);
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .w(rems(4.5))
                            .flex_none()
                            .text_body()
                            .text_color(pal.text_muted)
                            .child(tr!("onboard.stage", n => stage + 1)),
                    )
                    .child(
                        div()
                            .w(rems(10.))
                            .flex_none()
                            .child(NumberInput::new(input).small().disabled(writing)),
                    )
                    .child(
                        Radio::new(("onboard-default-stage", stage))
                            .label(tr!("onboard.default"))
                            .checked(is_default)
                            .disabled(writing)
                            .on_click(cx.listener(move |panel, _, _, cx| {
                                panel.edit(cx, |draft| draft.default_slot = slot);
                            })),
                    )
            }))
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("onboard.dpi_stages_description")),
            )
    }

    fn rate_section(
        draft: &Draft,
        rates: &[ReportRate],
        writing: bool,
        pal: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_2()
            .child(section_label(tr!("onboard.report_rate"), pal))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .children(rates.iter().map(|&rate| {
                        Button::new(("onboard-rate", usize::from(rate.into_inner())))
                            .compact()
                            .label(tr!("pointer.report_rate_hz", rate => rate))
                            .selected(draft.report_rate == Some(rate))
                            .disabled(writing)
                            .on_click(cx.listener(move |panel, _, _, cx| {
                                panel.edit(cx, |draft| draft.report_rate = Some(rate));
                            }))
                    })),
            )
    }

    fn lighting_section(
        draft: &Draft,
        writing: bool,
        pal: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let zones = draft.lighting.len();
        // The Lighting tab's card title already names the group.
        v_flex()
            .gap_3()
            .when(zones == 0, |this| {
                this.child(status_line(tr!("onboard.no_lighting"), pal))
            })
            .children(
                draft
                    .lighting
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(zone, lighting)| {
                        let current = Effect::of(lighting);
                        let color = match lighting {
                            OnboardLighting::Fixed(color)
                            | OnboardLighting::Breathing { color, .. } => Some(color),
                            _ => None,
                        };
                        v_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .gap_3()
                                    .items_center()
                                    .flex_wrap()
                                    .when(zones > 1, |this| {
                                        this.child(
                                            div()
                                                .w(rems(4.5))
                                                .flex_none()
                                                .text_body()
                                                .text_color(pal.text_muted)
                                                .child(tr!("onboard.zone", n => zone + 1)),
                                        )
                                    })
                                    .child(h_flex().gap_2().flex_wrap().children(
                                        Effect::ALL.into_iter().map(|effect| {
                                            Button::new((
                                                SharedString::from(format!(
                                                    "onboard-effect-{zone}"
                                                )),
                                                effect as usize,
                                            ))
                                            .compact()
                                            .label(effect.label())
                                            .selected(current == Some(effect))
                                            .disabled(writing)
                                            .on_click(
                                                cx.listener(move |panel, _, _, cx| {
                                                    panel.edit(cx, |draft| {
                                                        draft.lighting[zone] =
                                                            effect.apply(draft.lighting[zone]);
                                                    });
                                                }),
                                            )
                                        }),
                                    )),
                            )
                            .when(current.is_none(), |this| {
                                this.child(
                                    div()
                                        .text_caption()
                                        .text_color(pal.text_muted)
                                        .child(tr!("onboard.other_effect_description")),
                                )
                            })
                            .when_some(color, |this, selected| {
                                this.child(Self::swatch_row(
                                    zone, selected, zones, writing, pal, cx,
                                ))
                            })
                    }),
            )
    }

    /// The colors zone `zone` can take, `selected` among them — first, when
    /// it is not one of [`PALETTE`], so the color it holds is always shown.
    fn swatch_row(
        zone: usize,
        selected: Rgb,
        zones: usize,
        writing: bool,
        pal: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let panel = cx.entity();
        h_flex()
            .gap_2()
            .flex_wrap()
            .when(zones > 1, |this| this.pl(rems(5.25)))
            .children(
                (!PALETTE.contains(&selected))
                    .then_some(selected)
                    .into_iter()
                    .chain(PALETTE.iter().copied())
                    .map(move |swatch| {
                        let panel = panel.clone();
                        color_swatch(
                            (
                                SharedString::from(format!("onboard-swatch-{zone}")),
                                swatch.packed() as usize,
                            ),
                            swatch,
                            swatch == selected,
                            pal,
                            move |cx| {
                                if writing {
                                    return;
                                }
                                panel.update(cx, |panel, cx| {
                                    panel.edit(cx, |draft| {
                                        draft.lighting[zone] =
                                            with_color(draft.lighting[zone], swatch);
                                    });
                                });
                            },
                        )
                    }),
            )
    }

    fn footer(
        &self,
        facts: &Facts,
        profile: &OnboardProfile,
        dirty: bool,
        pal: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let status: Option<(SharedString, bool)> = if facts.writing {
            Some((tr!("onboard.saving"), false))
        } else if let Some(invalid) = &self.invalid {
            Some((invalid.clone(), true))
        } else if let Some(error) = &facts.refusal {
            Some((
                tr!("onboard.write_failed", error => error.to_string()),
                true,
            ))
        } else if dirty {
            Some((tr!("onboard.unsaved_changes"), false))
        } else {
            None
        };
        let sensor = facts.sensor.clone();
        let latest = profile.clone();
        h_flex()
            .gap_2()
            .items_center()
            .pt_4()
            .border_t_1()
            .border_color(pal.border)
            .child(div().flex_1().min_w_0().text_caption().when_some(
                status,
                |this, (text, error)| {
                    this.text_color(if error {
                        cx.theme().danger
                    } else {
                        pal.text_muted
                    })
                    .child(text)
                },
            ))
            .child(
                control_button("onboard-revert")
                    .outline()
                    .label(tr!("onboard.revert"))
                    .disabled(!dirty || facts.writing)
                    .on_click(cx.listener(|panel, _, window, cx| {
                        if let Some(draft) = panel.draft.clone() {
                            panel.reset(&draft.key, &draft.base, window, cx);
                            cx.notify();
                        }
                    })),
            )
            .child(
                control_button("onboard-save")
                    .primary()
                    .label(tr!("onboard.save_to_mouse"))
                    .disabled(!dirty || facts.writing)
                    .on_click(cx.listener(move |panel, _, _, cx| {
                        panel.save(&latest, sensor.as_ref(), cx);
                    })),
            )
    }
}
