//! The onboard DPI of a Logitech G600, which keeps four DPI stages in each of
//! its three onboard profiles and switches between them with its DPI button.
//!
//! The panel switches the active profile's stage at once, and edits one
//! profile's stages as dots on a single DPI track — up to four distinct
//! DPIs and the one the profile starts on — as a draft that a single Save
//! stores in the mouse, lowest DPI first, through
//! [`AppState::commit_g600_profile_dpi`]. The mouse
//! takes over a second to store a profile, so every control waits while a
//! write is in flight and until the mouse has been read again, and the
//! reading it gives then replaces the draft it confirms. A draft left by
//! viewing another profile is kept until that profile is viewed again.
//! Saving the profile in use keeps the mouse on the stage it is using,
//! wherever the save puts it. A draft also outlasts the mouse going offline
//! and another device being selected. While the card shows a mouse, it reads
//! it again now and then, since the mouse's own buttons switch its stage and
//! profile without a word to the agent.

mod draft;

use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Task, WeakEntity, Window, div, px,
};
use gpui_kit::component::{
    Disableable as _, Selectable as _,
    button::{Button, ButtonGroup, ButtonVariants as _},
    h_flex,
    input::{InputEvent, InputState},
    menu::{PopupMenu, PopupMenuItem},
    v_flex,
};
use openlogi_core::hid::g600::{
    DpiSettingsError, DpiStage, DpiState, DpiValue, OnboardProfile, ProfileDpi, StageAfterStore,
};

use self::draft::{G600Draft, save_reorders};
use crate::state::{
    AppState, DeviceKey, G600_RECONCILE_INTERVAL, G600DpiLoad, G600WriteStatus, Load, StateEvent,
};
use crate::ui::components::{control_button, control_input, stacked_row};
use crate::ui::log_track::{LogTrack, Nudge, TrackThumb, doubling_ticks, filled_dot, mark_swatch};
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, SelectableStyle as _, Typography as _};

/// The G600 onboard-DPI card's body, kept across frames for its drafts and
/// the last reading of the mouse.
pub struct G600Panel {
    /// The profile chosen for editing on each G600, until another is chosen;
    /// the active profile otherwise.
    viewed: Option<(DeviceKey, OnboardProfile)>,
    editor: Option<Editor>,
    /// Unsaved drafts of profiles not on view, until they are viewed again.
    stashed: Vec<Stashed>,
    /// The mouse's last reading, still shown while a read after it fails.
    last_reading: Option<(DeviceKey, Arc<DpiState>)>,
    /// The next read of the mouse on view, while the card shows it.
    reconcile: Option<Task<()>>,
    _state_obs: Subscription,
}

/// An unsaved draft of a profile that is not on view.
struct Stashed {
    key: DeviceKey,
    profile: OnboardProfile,
    base: ProfileDpi,
    draft: G600Draft,
}

/// One profile under edit, with the reading it started from.
struct Editor {
    key: DeviceKey,
    profile: OnboardProfile,
    /// The profile as the mouse last reported it.
    base: ProfileDpi,
    draft: G600Draft,
    /// The stage the DPI field, the buttons beside it, and the arrow keys
    /// act on, by its DPI, which no other stage shares. The default stage
    /// when none was picked or the picked one is gone.
    selected: Option<DpiValue>,
    /// The focus the DPI track takes, so its arrow keys work.
    track_focus: FocusHandle,
    /// The selected stage's DPI, to type.
    dpi_input: Entity<InputState>,
    /// The DPI of the stage the field was last filled from, which what is
    /// typed into it goes to: the field commits when it loses focus, after
    /// the frame that may already have selected another stage.
    dpi_field: Option<DpiValue>,
    _dpi_input_events: Subscription,
}

impl Editor {
    fn new(
        key: DeviceKey,
        profile: OnboardProfile,
        base: ProfileDpi,
        draft: G600Draft,
        window: &mut Window,
        cx: &mut Context<G600Panel>,
    ) -> Self {
        let dpi_input = cx.new(|cx| InputState::new(window, cx));
        let dpi_input_events = cx.subscribe_in(
            &dpi_input,
            window,
            |panel, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    panel.commit_typed_dpi(window, cx);
                }
            },
        );
        Self {
            key,
            profile,
            base,
            draft,
            selected: None,
            track_focus: cx.focus_handle().tab_stop(true),
            dpi_input,
            dpi_field: None,
            _dpi_input_events: dpi_input_events,
        }
    }

    /// Take the mouse's new `reading` of the profile: it replaces the draft
    /// unless the draft holds edits the reading does not.
    fn take_reading(&mut self, reading: ProfileDpi) {
        if self.base == reading {
            return;
        }
        if self.draft.matches(&self.base) || self.draft.matches(&reading) {
            self.reset(reading);
        } else {
            self.base = reading;
        }
    }

    /// Start the draft over from `profile`.
    fn reset(&mut self, profile: ProfileDpi) {
        self.base = profile;
        self.draft = G600Draft::from_profile(&profile);
        self.selected = None;
    }

    fn changed(&self) -> bool {
        !self.draft.matches(&self.base)
    }

    /// Where the selected stage sits on the track, if the profile has one.
    fn selected_index(&self) -> Option<usize> {
        let stages = self.draft.stages();
        self.selected
            .and_then(|dpi| stages.iter().position(|stage| *stage == dpi))
            .or_else(|| (!stages.is_empty()).then(|| self.draft.default_index()))
    }

    fn add(&mut self, dpi: DpiValue) {
        if self.draft.add(dpi).is_some() {
            self.selected = Some(dpi);
        }
    }

    fn move_stage(&mut self, index: usize, dpi: DpiValue) {
        if let Some(moved) = self.draft.move_stage(index, dpi) {
            self.selected = Some(moved);
        }
    }

    fn nudge(&mut self, index: usize, nudge: Nudge) {
        if let Some(moved) = self.draft.nudge(index, nudge) {
            self.selected = Some(moved);
        }
    }

    fn remove(&mut self, index: usize) {
        if let Some(nearest) = self.draft.remove(index) {
            self.selected = self.draft.stages().get(nearest).copied();
        }
    }

    fn select(&mut self, index: usize) {
        if let Some(dpi) = self.draft.stages().get(index) {
            self.selected = Some(*dpi);
        }
    }

    /// Whether the field holds a DPI typed for its stage and not yet put
    /// on it.
    fn dpi_typed(&self, cx: &App) -> bool {
        self.dpi_field.is_some_and(|dpi| {
            self.draft.stages().contains(&dpi)
                && *self.dpi_input.read(cx).value() != *dpi.get().to_string()
        })
    }

    /// Show the selected stage's DPI in its field, unless someone is typing
    /// in it or what they typed has yet to be put on its stage.
    fn sync_dpi_input(&mut self, window: &mut Window, cx: &mut Context<G600Panel>) {
        if self.dpi_input.focus_handle(cx).is_focused(window) || self.dpi_typed(cx) {
            return;
        }
        self.fill_dpi_input(window, cx);
    }

    /// Fill the field with the selected stage's DPI.
    fn fill_dpi_input(&mut self, window: &mut Window, cx: &mut Context<G600Panel>) {
        let Some(index) = self.selected_index() else {
            return;
        };
        let dpi = self.draft.stages()[index];
        self.dpi_field = Some(dpi);
        let shown = SharedString::from(dpi.get().to_string());
        if self.dpi_input.read(cx).value() != shown {
            self.dpi_input
                .update(cx, |input, cx| input.set_value(shown, window, cx));
        }
    }
}

impl G600Panel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs =
            AppState::repaint_on(cx, |event| matches!(event, StateEvent::G600DpiChanged(_)));
        Self {
            viewed: None,
            editor: None,
            stashed: Vec::new(),
            last_reading: None,
            reconcile: None,
            _state_obs: state_obs,
        }
    }

    fn edit(&mut self, change: impl FnOnce(&mut Editor), cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.editor {
            change(editor);
            cx.notify();
        }
    }

    /// Put the DPI typed into the field on the stage it was filled from:
    /// the nearest DPI the mouse can store, short of its neighbours. That
    /// stage stays selected unless another one was selected meanwhile. A
    /// field that holds no number goes back to the stage's DPI.
    fn commit_typed_dpi(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let typed = editor.dpi_input.read(cx).value();
        let typed = typed
            .trim()
            .parse::<u32>()
            .ok()
            .map(|dpi| DpiValue::nearest(u16::try_from(dpi).unwrap_or(u16::MAX)));
        let target = editor
            .dpi_field
            .and_then(|dpi| editor.draft.stages().iter().position(|stage| *stage == dpi));
        if let (Some(dpi), Some(index)) = (typed, target) {
            let was_selected = editor.selected_index() == Some(index);
            if let Some(moved) = editor.draft.move_stage(index, dpi)
                && was_selected
            {
                editor.selected = Some(moved);
            }
        }
        editor.fill_dpi_input(window, cx);
        cx.notify();
    }

    /// Keep the editor on the profile being viewed, stashing the draft of
    /// the one it leaves, and let a new reading replace the draft unless it
    /// holds edits the reading does not.
    fn sync_editor(
        &mut self,
        key: &DeviceKey,
        mouse: &DpiState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let profile = self
            .viewed
            .as_ref()
            .filter(|(viewed, _)| viewed == key)
            .map_or(mouse.active.profile, |(_, profile)| *profile);
        let reading = *mouse.profile(profile);
        if let Some(editor) = &mut self.editor
            && editor.key == *key
            && editor.profile == profile
        {
            editor.take_reading(reading);
            return;
        }
        self.put_editor_away();
        let stashed = self
            .stashed
            .iter()
            .position(|stashed| stashed.key == *key && stashed.profile == profile)
            .map(|index| self.stashed.swap_remove(index));
        let mut editor = match stashed {
            Some(stashed) => Editor::new(
                key.clone(),
                profile,
                stashed.base,
                stashed.draft,
                window,
                cx,
            ),
            None => Editor::new(
                key.clone(),
                profile,
                reading,
                G600Draft::from_profile(&reading),
                window,
                cx,
            ),
        };
        editor.take_reading(reading);
        self.editor = Some(editor);
    }

    /// Close the editor, keeping a draft with edits until its profile is
    /// viewed again.
    fn put_editor_away(&mut self) {
        if let Some(left) = self.editor.take()
            && left.changed()
        {
            self.stashed.push(Stashed {
                key: left.key,
                profile: left.profile,
                base: left.base,
                draft: left.draft,
            });
        }
    }

    /// Stop showing a mouse: keep its draft, and stop reading it.
    fn leave_mouse(&mut self) {
        self.put_editor_away();
        self.reconcile = None;
    }

    /// Read `key`'s mouse again after a while unless that is already due.
    /// The read repaints the card, whose next frame asks for the one after,
    /// so the reads stop with the frames once the card is no longer drawn.
    fn schedule_reconcile(&mut self, key: &DeviceKey, cx: &mut Context<Self>) {
        if self.reconcile.is_some() {
            return;
        }
        let key = key.clone();
        self.reconcile = Some(cx.spawn(async move |panel, cx| {
            cx.background_executor()
                .timer(G600_RECONCILE_INTERVAL)
                .await;
            let _ = panel.update(cx, |panel, cx| {
                panel.reconcile = None;
                AppState::update(cx, |state, _| state.reconcile_g600_dpi(&key));
                cx.notify();
            });
        }));
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else {
            return;
        };
        let Ok(settings) = editor.draft.settings() else {
            return;
        };
        let profile = editor.profile;
        let after = self
            .last_reading
            .as_ref()
            .filter(|(key, _)| *key == editor.key)
            .map_or(StageAfterStore::Keep, |(_, mouse)| {
                editor.draft.stage_after_save(profile, mouse)
            });
        AppState::apply(cx, |state| {
            state.commit_g600_profile_dpi(profile, settings, after)
        });
    }

    fn revert(&mut self, cx: &mut Context<Self>) {
        self.edit(
            |editor| {
                let base = editor.base;
                editor.reset(base);
            },
            cx,
        );
    }
}

/// What the card needs about the selected G600.
struct G600Facts {
    key: DeviceKey,
    load: G600DpiLoad,
    reachable: bool,
    write: Option<G600WriteStatus>,
}

impl G600Facts {
    fn current(cx: &gpui::App) -> Option<Self> {
        let state = AppState::try_read(cx)?;
        if !state.current_g600_dpi_supported() {
            return None;
        }
        Some(Self {
            key: state.current_record()?.device_key(),
            load: state.current_g600_dpi_load(),
            reachable: state.current_g600_route().is_some(),
            write: state.current_g600_write(),
        })
    }

    fn busy(&self) -> bool {
        self.write.as_ref().is_some_and(G600WriteStatus::in_flight)
    }
}

impl Render for G600Panel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let Some(facts) = G600Facts::current(cx) else {
            self.leave_mouse();
            return v_flex().w_full().into_any_element();
        };
        if let Load::Ready(mouse) = &facts.load {
            self.last_reading = Some((facts.key.clone(), mouse.clone()));
        }
        // A failed read after a good one keeps the card, and its draft, on
        // the last reading.
        let shown = match &facts.load {
            Load::Ready(_) | Load::Failed(_) => self
                .last_reading
                .as_ref()
                .filter(|(key, _)| *key == facts.key)
                .map(|(_, mouse)| mouse.clone()),
            _ => None,
        };
        let Some(mouse) = shown.filter(|_| facts.reachable) else {
            self.leave_mouse();
            return load_status(&facts.load, &facts, pal).into_any_element();
        };
        self.sync_editor(&facts.key, &mouse, window, cx);
        self.schedule_reconcile(&facts.key, cx);
        let Some(editor) = &mut self.editor else {
            return v_flex().w_full().into_any_element();
        };
        editor.sync_dpi_input(window, cx);
        let editor = &*editor;
        let read_failed =
            matches!(facts.load, Load::Failed(_)).then(|| read_retry(&facts).into_any_element());
        let profile = profile_row(editor.profile, &mouse, facts.busy(), pal, cx).into_any_element();
        let stages = stages_section(editor, &mouse, &facts, pal, cx).into_any_element();
        let save = save_row(editor, &facts, pal, cx).into_any_element();
        v_flex()
            .w_full()
            .gap_4()
            .children(read_failed)
            .child(active_stage_row(&mouse, &facts, pal, cx))
            .child(div().w_full().h_px().bg(pal.border))
            .child(profile)
            .child(stages)
            .child(save)
            .into_any_element()
    }
}

/// What the card shows until it has a reading of a reachable mouse.
fn load_status(load: &G600DpiLoad, facts: &G600Facts, pal: Palette) -> AnyElement {
    if !facts.reachable {
        return status_line(tr!("pointer.device_offline_dpi_is_unavailable"), pal)
            .into_any_element();
    }
    match load {
        Load::Unknown | Load::Loading | Load::Ready(_) => {
            status_line(tr!("pointer.g600_reading"), pal).into_any_element()
        }
        Load::Failed(_) => read_retry(facts).into_any_element(),
        Load::Unsupported(message) => status_line(message.clone(), pal).into_any_element(),
    }
}

/// That reading the mouse failed, with a retry.
fn read_retry(facts: &G600Facts) -> impl IntoElement {
    let key = facts.key.clone();
    retry_line(
        "g600-retry",
        tr!("pointer.g600_read_failed_retry"),
        move |cx| {
            AppState::apply(cx, |state| state.retry_g600_dpi_read(&key));
        },
    )
}

/// The stage the mouse uses now, switched at once like its DPI button does.
fn active_stage_row(
    mouse: &DpiState,
    facts: &G600Facts,
    pal: Palette,
    cx: &mut Context<G600Panel>,
) -> impl IntoElement {
    let active = mouse.active;
    let stored = *mouse.profile(active.profile);
    let busy = facts.busy();
    let group = ButtonGroup::new("g600-active-stage")
        .w_full()
        .outline()
        .disabled(busy)
        .children(DpiStage::ALL.map(|stage| {
            let selected = active.dpi_stage == stage;
            let label = stored.dpi(stage).map_or_else(
                || tr!("pointer.g600_stage_off"),
                |dpi| tr!("pointer.g600_stage_dpi", dpi => dpi),
            );
            Button::new(("g600-active-stage", usize::from(stage.number())))
                .debug_selector(move || format!("g600-active-stage-{}", stage.number()))
                .flex_1()
                .label(label)
                .tooltip(tr!("pointer.g600_stage", number => stage.number()))
                .disabled(busy || !stored.is_enabled(stage))
                .selected(selected)
                .selected_segment(selected && !busy, cx)
        }))
        .on_click(cx.listener(move |_, indices: &Vec<usize>, _, cx| {
            let Some(stage) = indices.first().and_then(|index| DpiStage::ALL.get(*index)) else {
                return;
            };
            // The stage marked here may be one the mouse's own button has
            // since left, so a click on it still switches.
            AppState::apply(cx, |state| state.commit_g600_active_stage(*stage));
        }));
    let failed = match &facts.write {
        Some(G600WriteStatus::StageFailed(error)) => Some(
            div()
                .text_caption()
                .text_color(pal.text_danger)
                .child(tr!("pointer.g600_write_failed", message => error.to_string())),
        ),
        _ => None,
    };
    v_flex()
        .w_full()
        .gap_2()
        .child(stacked_row(
            "g600-active-stage",
            pal,
            tr!("pointer.g600_active_stage"),
            tr!("pointer.g600_active_stage_description", profile => active.profile.number()),
            group,
        ))
        .children(failed)
}

/// The onboard profile whose stages are edited below.
fn profile_row(
    viewed: OnboardProfile,
    mouse: &DpiState,
    busy: bool,
    pal: Palette,
    cx: &mut Context<G600Panel>,
) -> impl IntoElement {
    let active = mouse.active.profile;
    let group = ButtonGroup::new("g600-profile")
        .w_full()
        .outline()
        .disabled(busy)
        .children(OnboardProfile::ALL.map(|profile| {
            let selected = profile == viewed;
            let label = if profile == active {
                tr!("pointer.g600_profile_in_use", number => profile.number())
            } else {
                tr!("pointer.onboard_profile_number", number => profile.number())
            };
            Button::new(("g600-profile", usize::from(profile.number())))
                .debug_selector(move || format!("g600-profile-{}", profile.number()))
                .flex_1()
                .label(label)
                .disabled(busy)
                .selected(selected)
                .selected_segment(selected && !busy, cx)
        }))
        .on_click(cx.listener(|panel, indices: &Vec<usize>, _, cx| {
            let Some(profile) = indices
                .first()
                .and_then(|index| OnboardProfile::ALL.get(*index))
            else {
                return;
            };
            let Some(key) = panel.editor.as_ref().map(|editor| editor.key.clone()) else {
                return;
            };
            panel.viewed = Some((key, *profile));
            cx.notify();
        }));
    stacked_row(
        "g600-profile",
        pal,
        tr!("pointer.g600_profile"),
        tr!("pointer.g600_profile_description"),
        group,
    )
}

/// The viewed profile's stages as dots on one DPI track, with what the
/// dots mean, and a row that edits the selected dot without the pointer.
fn stages_section(
    editor: &Editor,
    mouse: &DpiState,
    facts: &G600Facts,
    pal: Palette,
    cx: &mut Context<G600Panel>,
) -> impl IntoElement {
    let busy = facts.busy();
    let draft = &editor.draft;
    let selected = editor.selected_index();
    let in_use = editor.draft.in_use(editor.profile, mouse);
    let track = stages_track(editor, in_use, busy, cx);
    let full_hint = draft.is_full().then(|| {
        div()
            .debug_selector(|| "g600-stages-full".into())
            .text_caption()
            .text_color(pal.text_muted)
            .child(
                tr!("pointer.g600_stages_full", count => openlogi_core::hid::g600::DPI_STAGE_COUNT),
            )
    });
    let reordered = save_reorders(&editor.base).then(|| {
        div()
            .debug_selector(|| "g600-stages-reordered".into())
            .text_caption()
            .text_color(pal.text_muted)
            .child(tr!("pointer.g600_stages_reordered"))
    });
    v_flex()
        .w_full()
        .gap_2()
        .child(
            v_flex()
                .child(
                    div()
                        .text_body()
                        .text_color(pal.text_primary)
                        .child(tr!("pointer.g600_stages")),
                )
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(tr!("pointer.g600_stages_description")),
                ),
        )
        .child(track)
        .child(
            h_flex()
                .w_full()
                .flex_wrap()
                .gap_x_4()
                .gap_y_1()
                .child(legend_item(
                    LegendMark::Default,
                    tr!("pointer.g600_default_stage"),
                    pal,
                    cx,
                ))
                .children(in_use.map(|_| {
                    legend_item(LegendMark::InUse, tr!("pointer.g600_stage_in_use"), pal, cx)
                }))
                .child(div().flex_1())
                .children(full_hint),
        )
        .children(reordered)
        .children(selected.map(|index| selected_row(editor, index, busy, pal, cx)))
}

/// The DPI track: a dot per stage, filled for the default, ringed when
/// selected, marked when the mouse is using it now.
fn stages_track(
    editor: &Editor,
    in_use: Option<usize>,
    busy: bool,
    cx: &mut Context<G600Panel>,
) -> LogTrack<DpiValue> {
    let draft = &editor.draft;
    let default = draft.default_index();
    let selected = editor.selected_index();
    let panel = cx.entity().downgrade();
    let thumbs = draft.stages().iter().enumerate().map(|(index, dpi)| {
        TrackThumb::new(*dpi, dpi.get().to_string())
            .filled(index == default)
            .selected(Some(index) == selected)
            .marked(Some(index) == in_use)
    });
    let ticks = doubling_ticks(DpiValue::MIN, DpiValue::MAX)
        .into_iter()
        .map(|tick| {
            (
                DpiValue::nearest(tick),
                SharedString::from(tick.to_string()),
            )
        });
    LogTrack::new(
        "g600-dpi",
        DpiValue::nearest(DpiValue::MIN),
        DpiValue::nearest(DpiValue::MAX),
    )
    .ticks(ticks)
    .thumbs(thumbs)
    .track_focus(&editor.track_focus)
    .disabled(busy)
    .accessibility_label(tr!("pointer.g600_stages"))
    .on_add({
        let panel = panel.clone();
        move |dpi, _, cx| edit_panel(&panel, cx, |editor| editor.add(dpi))
    })
    .on_move({
        let panel = panel.clone();
        move |index, dpi, _, cx| edit_panel(&panel, cx, |editor| editor.move_stage(index, dpi))
    })
    .on_select({
        let panel = panel.clone();
        move |index, _, cx| edit_panel(&panel, cx, |editor| editor.select(index))
    })
    .on_nudge({
        let panel = panel.clone();
        move |index, nudge, _, cx| edit_panel(&panel, cx, |editor| editor.nudge(index, nudge))
    })
    .thumb_menu(move |index, menu, _, cx| stage_menu(&panel, menu, index, cx))
}

/// Change the editor of the panel behind `panel`, from a callback that
/// holds no `Context` of it.
fn edit_panel(panel: &WeakEntity<G600Panel>, cx: &mut App, change: impl FnOnce(&mut Editor)) {
    // A panel that is gone has nothing to edit.
    panel.update(cx, |panel, cx| panel.edit(change, cx)).ok();
}

/// What stage `index` of a draft can be told to do, the same from its menu
/// and from the buttons beside its field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct StageCommands {
    /// It is not the default stage already.
    can_default: bool,
    /// It is not the last stage, which the mouse needs to start on.
    removable: bool,
}

impl StageCommands {
    fn of(draft: &G600Draft, index: usize) -> Self {
        let len = draft.stages().len();
        Self {
            can_default: index < len && index != draft.default_index(),
            removable: index < len && len > 1,
        }
    }
}

/// The menu of stage `index`: make it the default, or remove it.
fn stage_menu(panel: &WeakEntity<G600Panel>, menu: PopupMenu, index: usize, cx: &App) -> PopupMenu {
    let commands = panel
        .upgrade()
        .and_then(|panel| {
            let editor = panel.read(cx).editor.as_ref()?;
            Some(StageCommands::of(&editor.draft, index))
        })
        .unwrap_or_default();
    let default_panel = panel.clone();
    let remove_panel = panel.clone();
    menu.item(
        PopupMenuItem::new(tr!("pointer.g600_set_default_stage"))
            .disabled(!commands.can_default)
            .on_click(move |_, _, cx| {
                edit_panel(&default_panel, cx, |editor| editor.draft.set_default(index));
            }),
    )
    .item(
        PopupMenuItem::new(tr!("pointer.g600_remove_stage"))
            .disabled(!commands.removable)
            .on_click(move |_, _, cx| {
                edit_panel(&remove_panel, cx, |editor| editor.remove(index));
            }),
    )
}

/// What a legend entry shows beside its text.
#[derive(Clone, Copy)]
enum LegendMark {
    /// A filled dot, like the default stage's.
    Default,
    /// The small mark under the stage in use.
    InUse,
}

fn legend_item(mark: LegendMark, text: SharedString, pal: Palette, cx: &App) -> impl IntoElement {
    use gpui_kit::component::ActiveTheme as _;
    let accent = cx.theme().primary;
    let (swatch, selector) = match mark {
        LegendMark::Default => (filled_dot(accent), "g600-legend-default"),
        LegendMark::InUse => (mark_swatch(accent), "g600-legend-in-use"),
    };
    h_flex()
        .debug_selector(move || selector.into())
        .gap_1p5()
        .items_center()
        .child(div().w(px(14.)).flex().justify_center().child(swatch))
        .child(div().text_caption().text_color(pal.text_muted).child(text))
}

/// The selected stage's DPI to type, and its commands, so every edit the
/// track's pointer and menu offer is also a field and two buttons.
fn selected_row(
    editor: &Editor,
    index: usize,
    busy: bool,
    pal: Palette,
    cx: &mut Context<G600Panel>,
) -> impl IntoElement {
    let commands = StageCommands::of(&editor.draft, index);
    h_flex()
        .w_full()
        .flex_wrap()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_body()
                .text_color(pal.text_primary)
                .child(tr!("pointer.g600_selected_stage")),
        )
        .child(
            div()
                .w(px(88.))
                .debug_selector(|| "g600-selected-dpi".into())
                .child(control_input(&editor.dpi_input).disabled(busy)),
        )
        .child(
            div()
                .text_body()
                .text_color(pal.text_muted)
                .child(tr!("pointer.g600_dpi_unit")),
        )
        .child(div().flex_1())
        .child(
            control_button("g600-set-default")
                .debug_selector(|| "g600-set-default".into())
                .outline()
                .label(tr!("pointer.g600_set_default"))
                .disabled(busy || !commands.can_default)
                .on_click(cx.listener(move |panel, _: &ClickEvent, _, cx| {
                    panel.edit(|editor| editor.draft.set_default(index), cx);
                })),
        )
        .child(
            control_button("g600-remove")
                .debug_selector(|| "g600-remove".into())
                .outline()
                .label(tr!("pointer.g600_remove"))
                .disabled(busy || !commands.removable)
                .on_click(cx.listener(move |panel, _: &ClickEvent, _, cx| {
                    panel.edit(|editor| editor.remove(index), cx);
                })),
        )
}

/// Why Save is off, or how the last write went, beside Revert and Save.
fn save_row(
    editor: &Editor,
    facts: &G600Facts,
    pal: Palette,
    cx: &mut Context<G600Panel>,
) -> impl IntoElement {
    let settings = editor.draft.settings();
    let changed = editor.changed();
    let busy = facts.busy();
    let saving = matches!(
        facts.write,
        Some(G600WriteStatus::SavingProfile(profile)) if profile == editor.profile
    );
    let note = save_note(
        editor.profile,
        settings.err(),
        changed,
        facts.write.as_ref(),
    );
    h_flex()
        .w_full()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_caption()
                .children(note.map(|(text, danger)| {
                    div()
                        .text_color(if danger {
                            pal.text_danger
                        } else {
                            pal.text_muted
                        })
                        .child(text)
                })),
        )
        .child(
            control_button("g600-revert")
                .outline()
                .label(tr!("pointer.g600_revert"))
                .disabled(busy || !changed)
                .on_click(cx.listener(|panel, _: &ClickEvent, _, cx| panel.revert(cx))),
        )
        .child(
            control_button("g600-save")
                .debug_selector(|| "g600-save".into())
                .primary()
                .label(tr!("pointer.g600_save"))
                .loading(saving)
                .disabled(busy || !changed || settings.is_err())
                .on_click(cx.listener(|panel, _: &ClickEvent, _, cx| panel.save(cx))),
        )
}

/// The line beside Save for `viewed`, and whether it reports a problem: a
/// write in flight or the mouse being read after it, what keeps the draft
/// from being saved, the failure of the last write to this profile, or that
/// the draft holds unsaved changes.
fn save_note(
    viewed: OnboardProfile,
    invalid: Option<DpiSettingsError>,
    changed: bool,
    write: Option<&G600WriteStatus>,
) -> Option<(SharedString, bool)> {
    match (write, invalid) {
        (Some(G600WriteStatus::SavingProfile(_)), _) => Some((tr!("pointer.g600_saving"), false)),
        (Some(G600WriteStatus::SelectingStage(_)), _) => {
            Some((tr!("pointer.g600_selecting_stage"), false))
        }
        (Some(G600WriteStatus::Confirming), _) => Some((tr!("pointer.g600_reading"), false)),
        // A draft always starts on one of its stages, so the one reason it
        // cannot be stored is that it has none.
        (_, Some(DpiSettingsError::NoStageEnabled | DpiSettingsError::DefaultStageDisabled(_))) => {
            Some((tr!("pointer.g600_no_stage_enabled"), true))
        }
        (Some(G600WriteStatus::ProfileFailed(profile, error)), None) if *profile == viewed => {
            Some((
                tr!("pointer.g600_write_failed", message => error.to_string()),
                true,
            ))
        }
        (_, None) if changed => Some((tr!("pointer.g600_unsaved"), false)),
        (_, None) => None,
    }
}

#[cfg(test)]
mod tests;
