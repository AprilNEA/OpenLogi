//! The card in a window: the DPI track edits a draft that stays local until
//! Save, and Save stores it lowest DPI first.

use std::sync::Arc;

use gpui::{
    AppContext as _, Modifiers, MouseButton, Pixels, Point, VisualTestContext, point, px, size,
};
use gpui_kit::component::Root;
use openlogi_core::config::Config;
use openlogi_core::device::{Capabilities, DeviceKind, RawDeviceAddress, StandaloneDevice};
use openlogi_core::hid::WriteError;
use openlogi_core::hid::g600::{
    ActiveSelection, DpiSettings, DpiStage, DpiState, DpiValue, G600_DRIVER_ID, OnboardProfile,
    ProfileDpi, StageAfterStore,
};
use tokio::sync::mpsc::UnboundedReceiver;

use super::G600Panel;
use crate::services::assets::AssetResolver;
use crate::services::i18n::LOCALE_LOCK;
use crate::services::ipc::{Command, ReadG600Dpi, SetG600ProfileDpi};
use crate::state::{AppState, Sources};
use crate::ui::log_track::geometry::LogScale;
use openlogi_core::hid::HidppOperation;

fn g600() -> StandaloneDevice {
    StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: 0x046d,
            product_id: 0xc24a,
            usage_page: 0xff80,
            usage_id: 0x0080,
            identity: "serial:g600-panel".into(),
        },
        display_name: "G600 Gaming Mouse".into(),
        manufacturer: Some("Logitech".into()),
        serial_number: None,
        unit_id: [0; 4],
        kind: DeviceKind::Mouse,
        online: true,
        capabilities: Some(Capabilities::default()),
        light_capabilities: None,
        driver_id: G600_DRIVER_ID.into(),
        registry_model_id: None,
    }
}

fn stage(number: u8) -> DpiStage {
    DpiStage::from_number(number).expect("a stage")
}

/// Profile 1 on stage 1. Profiles 1 and 3 have 800 and 1600 DPI on their
/// first two stages and start on the first; profile 2 holds 3200 before
/// 800 and starts on 800.
fn reading() -> DpiState {
    let stored = ProfileDpi {
        stages: [Some(800), Some(1600), None, None],
        default_stage: Some(stage(1)),
        shift: None,
    };
    let reversed = ProfileDpi {
        stages: [Some(3200), Some(800), None, None],
        default_stage: Some(stage(2)),
        shift: None,
    };
    DpiState {
        active: ActiveSelection {
            profile: OnboardProfile::ALL[0],
            dpi_stage: stage(1),
        },
        profiles: [stored, reversed, stored],
    }
}

fn drain(receiver: &mut UnboundedReceiver<Command>) -> Vec<Command> {
    std::iter::from_fn(|| receiver.try_recv().ok()).collect()
}

/// The G600 writes sent since the last call; the window's other reads are
/// not the card's business.
fn writes(receiver: &mut UnboundedReceiver<Command>) -> Vec<Command> {
    drain(receiver)
        .into_iter()
        .filter(|command| {
            matches!(
                command,
                Command::SetG600ProfileDpi(_) | Command::SetG600ActiveStage(_)
            )
        })
        .collect()
}

/// The card on a window, with the agent's reading of the mouse landed.
fn open_card(
    cx: &mut gpui::TestAppContext,
) -> (UnboundedReceiver<Command>, &mut VisualTestContext) {
    rust_i18n::set_locale("en");
    // The catalog backend is built on first lookup, in one stack frame of
    // nearly 2 MB in a debug build: the whole stack of a test thread. Build
    // it here, before a window's draw is underneath it.
    let _ = tr!("pointer.g600_save");
    cx.update(gpui_kit::init);
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    cx.update(|cx| {
        let resolver = AssetResolver::new();
        let runtime: Arc<dyn swr_core::Runtime> = Arc::new(swr_gpui::GpuiRuntime::new(cx));
        let client = swr_core::SwrClient::builder().build(runtime.clone());
        let state = cx.new(|_| {
            let mut state = AppState::new(Sources {
                standalone: &[g600()],
                ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
            });
            state.connect_device_reads(client, runtime);
            state
        });
        AppState::set_global(state, cx);
        AppState::load_current_device_reads(cx);
    });
    cx.run_until_parked();
    for command in drain(&mut receiver) {
        if let Command::ReadG600Dpi(ReadG600Dpi { reply, .. }) = command {
            let _ = reply.send(Ok(reading()));
        }
    }
    cx.run_until_parked();
    let (_, visual) = cx.add_window_view(|window, cx| {
        let panel = cx.new(G600Panel::new);
        Root::new(panel, window, cx)
    });
    visual.simulate_resize(size(px(900.), px(1200.)));
    redraw(visual);
    (receiver, visual)
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("the card shows {selector}"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn redraw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

/// The window point over `dpi` on the DPI track, level with its dots.
fn over(cx: &mut VisualTestContext, dpi: u16) -> Point<Pixels> {
    let rail = cx
        .debug_bounds("g600-dpi-rail")
        .expect("the card shows the DPI track");
    let fraction =
        LogScale::new(f32::from(DpiValue::MIN), f32::from(DpiValue::MAX)).fraction(f32::from(dpi));
    point(
        rail.left() + rail.size.width * fraction,
        rail.top() + px(30.),
    )
}

/// The centre of dot `index` on the track, lowest DPI first.
fn dot(cx: &mut VisualTestContext, index: usize) -> Point<Pixels> {
    cx.debug_bounds(Box::leak(
        format!("g600-dpi-thumb-{index}").into_boxed_str(),
    ))
    .unwrap_or_else(|| panic!("the track shows dot {index}"))
    .center()
}

fn click_track(cx: &mut VisualTestContext, dpi: u16) {
    let at = over(cx, dpi);
    cx.simulate_click(at, Modifiers::none());
    redraw(cx);
}

fn click_dot(cx: &mut VisualTestContext, index: usize) {
    let at = dot(cx, index);
    cx.simulate_click(at, Modifiers::none());
    redraw(cx);
}

/// Right-click dot `index` and pick item `item` of its menu, counting from 1:
/// 1 sets it as the default stage, 2 removes it.
fn dot_menu(cx: &mut VisualTestContext, index: usize, item: usize) {
    let at = dot(cx, index);
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
    redraw(cx);
    let keys = vec!["down"; item].join(" ");
    cx.simulate_keystrokes(&format!("{keys} enter"));
    redraw(cx);
}

fn dpi(value: u16) -> DpiValue {
    DpiValue::new(value).expect("a G600 DPI")
}

/// `stages` from the first slot on, the rest off, starting on `default`.
fn settings(stages: &[u16], default: u8) -> DpiSettings {
    let slots = std::array::from_fn(|slot| stages.get(slot).map(|value| dpi(*value)));
    DpiSettings::new(slots, stage(default)).expect("valid settings")
}

/// The stages a write stores, from the first slot on.
fn stored(settings: &DpiSettings) -> Vec<u16> {
    DpiStage::ALL
        .into_iter()
        .filter_map(|stage| settings.dpi(stage).map(DpiValue::get))
        .collect()
}

#[gpui::test]
fn save_waits_for_an_edit_and_then_for_the_mouse(cx: &mut gpui::TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (mut receiver, cx) = open_card(cx);

    click(cx, "g600-save");
    assert!(
        writes(&mut receiver).is_empty(),
        "an unedited profile has nothing to save"
    );

    // Removing the default stage, 800, makes its nearest neighbour the default.
    dot_menu(cx, 0, 2);
    click(cx, "g600-save");
    assert_eq!(
        profile_write(&mut receiver),
        (OnboardProfile::ALL[0], settings(&[1600], 1))
    );

    // The mouse is busy storing it: a second Save sends nothing.
    click(cx, "g600-save");
    assert!(writes(&mut receiver).is_empty());
}

#[gpui::test]
fn a_click_on_empty_track_adds_a_stage_in_order(cx: &mut gpui::TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (mut receiver, cx) = open_card(cx);

    click_track(cx, 400);
    click(cx, "g600-save");

    let (profile, written) = profile_write(&mut receiver);
    assert_eq!(profile, OnboardProfile::ALL[0]);
    let values = stored(&written);
    assert_eq!(values.len(), 3, "{values:?}");
    assert!(values[0].abs_diff(400) <= 50, "{values:?}");
    assert_eq!(values[1..], [800, 1600], "the new stage goes first");
    assert_eq!(
        written.default_stage(),
        stage(2),
        "the default stays on 800, now the second stage"
    );
}

#[gpui::test]
fn a_full_profile_takes_no_fifth_stage_and_says_why(cx: &mut gpui::TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (mut receiver, cx) = open_card(cx);
    assert!(cx.debug_bounds("g600-stages-full").is_none());

    click_track(cx, 3200);
    click_track(cx, 6400);
    assert!(
        cx.debug_bounds("g600-stages-full").is_some(),
        "four stages: the card says the mouse holds no more"
    );
    click_track(cx, 400);
    click(cx, "g600-save");

    let values = stored(&profile_write(&mut receiver).1);
    assert_eq!(values.len(), 4, "{values:?}");
    assert_eq!(values[..2], [800, 1600]);
    assert!(
        values[2].abs_diff(3200) <= 50 && values[3].abs_diff(6400) <= 100,
        "{values:?}"
    );
}

#[gpui::test]
fn dragging_a_dot_changes_its_dpi_in_steps_the_mouse_can_store(cx: &mut gpui::TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (mut receiver, cx) = open_card(cx);

    let from = dot(cx, 1);
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    for target in [2000, 2400] {
        let at = over(cx, target);
        cx.simulate_mouse_move(at, MouseButton::Left, Modifiers::none());
    }
    let at = over(cx, 2400);
    cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
    redraw(cx);
    click(cx, "g600-save");

    let values = stored(&profile_write(&mut receiver).1);
    assert_eq!(values[0], 800, "{values:?}");
    assert!(values[1].abs_diff(2400) <= 50, "{values:?}");
    assert_eq!(values[1] % DpiValue::STEP, 0);
}

#[gpui::test]
fn a_dot_dragged_onto_another_stops_short_of_it(cx: &mut gpui::TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (mut receiver, cx) = open_card(cx);

    let from = dot(cx, 0);
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    for target in [1200, 3200] {
        let at = over(cx, target);
        cx.simulate_mouse_move(at, MouseButton::Left, Modifiers::none());
    }
    let at = over(cx, 3200);
    cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
    redraw(cx);
    click(cx, "g600-save");

    assert_eq!(
        profile_write(&mut receiver).1,
        settings(&[1550, 1600], 1),
        "800 stops one step below 1600 and stays the default"
    );
}

#[gpui::test]
fn the_dot_menu_sets_the_default_stage(cx: &mut gpui::TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (mut receiver, cx) = open_card(cx);

    dot_menu(cx, 1, 1);
    click(cx, "g600-save");

    assert_eq!(profile_write(&mut receiver).1, settings(&[800, 1600], 2));
}

#[gpui::test]
fn the_selected_stage_row_edits_without_the_pointer(cx: &mut gpui::TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (mut receiver, cx) = open_card(cx);

    // A press on a dot selects it, and the arrow keys step it.
    click_dot(cx, 1);
    cx.simulate_keystrokes("right right");
    redraw(cx);
    click(cx, "g600-set-default");
    click(cx, "g600-save");
    assert_eq!(profile_write(&mut receiver).1, settings(&[800, 1700], 2));
}

#[gpui::test]
fn a_typed_dpi_is_snapped_to_one_the_mouse_can_store(cx: &mut gpui::TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (mut receiver, cx) = open_card(cx);

    click_dot(cx, 1);
    click(cx, "g600-selected-dpi");
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("2013");
    cx.simulate_keystrokes("enter");
    redraw(cx);
    click(cx, "g600-save");

    assert_eq!(profile_write(&mut receiver).1, settings(&[800, 2000], 1));
}

#[gpui::test]
fn stages_stored_out_of_order_are_saved_lowest_first(cx: &mut gpui::TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (mut receiver, cx) = open_card(cx);
    assert!(
        cx.debug_bounds("g600-stages-reordered").is_none(),
        "profile 1 is already in order"
    );

    click(cx, "g600-profile-2");
    assert!(
        cx.debug_bounds("g600-stages-reordered").is_some(),
        "the card says Save puts profile 2 in order"
    );
    click(cx, "g600-save");
    assert!(
        writes(&mut receiver).is_empty(),
        "putting it in order alone is no edit"
    );

    // 3200 becomes the default; 800 stays first.
    dot_menu(cx, 1, 1);
    click(cx, "g600-save");
    assert_eq!(
        profile_write(&mut receiver),
        (OnboardProfile::ALL[1], settings(&[800, 3200], 2))
    );
}

#[gpui::test]
fn another_profile_opens_its_own_stages(cx: &mut gpui::TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (mut receiver, cx) = open_card(cx);

    remove_stage_2(cx);
    click(cx, "g600-profile-3");
    click(cx, "g600-save");

    assert!(
        writes(&mut receiver).is_empty(),
        "the edit to profile 1 stays with profile 1, and profile 3 is unchanged"
    );
    click_track(cx, 3200);
    click(cx, "g600-save");
    let (profile, written) = profile_write(&mut receiver);
    assert_eq!(
        profile,
        OnboardProfile::ALL[2],
        "the write goes to the profile on view"
    );
    assert_eq!(stored(&written)[..2], [800, 1600]);
    assert_eq!(stored(&written).len(), 3);
}

/// The single profile write the card sent since the last call.
fn profile_write(receiver: &mut UnboundedReceiver<Command>) -> (OnboardProfile, DpiSettings) {
    let (profile, settings, _) = profile_write_after(receiver);
    (profile, settings)
}

/// The one profile write sent since the last call, with the stage it asks
/// the mouse to use afterwards.
fn profile_write_after(
    receiver: &mut UnboundedReceiver<Command>,
) -> (OnboardProfile, DpiSettings, StageAfterStore) {
    let commands = writes(receiver);
    let [
        Command::SetG600ProfileDpi(SetG600ProfileDpi {
            profile,
            settings,
            after,
            ..
        }),
    ] = commands.as_slice()
    else {
        panic!("expected one profile write, got {} writes", commands.len());
    };
    (*profile, *settings, *after)
}

/// Profile 1 with its second stage removed, as the card saves it.
fn without_stage_2() -> DpiSettings {
    settings(&[800], 1)
}

/// Select the 1600 DPI dot and remove it with the button beside its field.
fn remove_stage_2(cx: &mut VisualTestContext) {
    click_dot(cx, 1);
    click(cx, "g600-remove");
}

/// Answer every G600 read the card has queued with `answer`.
fn answer_reads(
    receiver: &mut UnboundedReceiver<Command>,
    answer: &Result<DpiState, WriteError>,
) -> usize {
    let mut answered = 0;
    for command in drain(receiver) {
        if let Command::ReadG600Dpi(ReadG600Dpi { reply, .. }) = command {
            let _ = reply.send(answer.clone());
            answered += 1;
        }
    }
    answered
}

#[gpui::test]
fn a_draft_left_for_another_profile_comes_back_with_it(cx: &mut gpui::TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (mut receiver, cx) = open_card(cx);

    remove_stage_2(cx);
    click(cx, "g600-profile-3");
    click(cx, "g600-profile-1");
    click(cx, "g600-save");

    assert_eq!(
        profile_write(&mut receiver),
        (OnboardProfile::ALL[0], without_stage_2())
    );
}

#[gpui::test]
fn a_failed_save_keeps_its_profile_and_draft_through_a_failed_read(cx: &mut gpui::TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (mut receiver, cx) = open_card(cx);
    let key = cx.update(|_, cx| {
        AppState::try_read(cx)
            .and_then(AppState::current_record)
            .expect("the G600 is selected")
            .device_key()
    });

    remove_stage_2(cx);
    click(cx, "g600-save");
    assert_eq!(
        profile_write(&mut receiver),
        (OnboardProfile::ALL[0], without_stage_2())
    );
    // The mouse is storing profile 1: another profile cannot be opened.
    click(cx, "g600-profile-3");

    let timed_out = WriteError::RequestTimedOut {
        operation: HidppOperation::WriteG600Profile,
    };
    cx.update(|_, cx| {
        AppState::apply(cx, |state| {
            state.apply_g600_dpi_written(&key, Err(timed_out))
        });
    });
    // The read after it fails too, every retry of it.
    let unreachable = Err(WriteError::AgentUnavailable);
    let mut reads = 0;
    for _ in 0..5 {
        cx.run_until_parked();
        reads += answer_reads(&mut receiver, &unreachable);
    }
    assert!(reads > 0, "the failed write reads the mouse again");
    assert!(cx.update(|_, cx| {
        AppState::try_read(cx).is_some_and(|state| {
            matches!(state.current_g600_dpi_load(), crate::state::Load::Failed(_))
        })
    }));
    cx.update(|window, cx| window.draw(cx).clear(cx));

    click(cx, "g600-save");
    assert_eq!(
        profile_write(&mut receiver),
        (OnboardProfile::ALL[0], without_stage_2()),
        "the draft and the profile on view outlast both failures"
    );
}

fn g600_key(cx: &mut VisualTestContext) -> crate::state::DeviceKey {
    cx.update(|_, cx| {
        AppState::try_read(cx)
            .and_then(AppState::current_record)
            .expect("the G600 is selected")
            .device_key()
    })
}

#[gpui::test]
fn saving_the_profile_in_use_keeps_the_mouse_on_the_stage_it_uses(cx: &mut gpui::TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (mut receiver, cx) = open_card(cx);
    let key = g600_key(cx);

    // The mouse is on stage 1, 800. A stage added below puts 800 in slot
    // 2, and the write itself asks the mouse to use slot 2.
    click_track(cx, 400);
    click(cx, "g600-save");
    let (_, written, after) = profile_write_after(&mut receiver);
    assert_eq!(written.dpi(stage(2)), Some(dpi(800)));
    assert_eq!(after, StageAfterStore::Use(stage(2)));
    cx.update(|_, cx| {
        AppState::apply(cx, |state| state.apply_g600_dpi_written(&key, Ok(())));
    });
    cx.run_until_parked();

    assert!(
        writes(&mut receiver).is_empty(),
        "no stage switch follows the save"
    );
}

#[gpui::test]
fn saving_another_profile_leaves_the_stage_in_use_alone(cx: &mut gpui::TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (mut receiver, cx) = open_card(cx);
    let key = g600_key(cx);

    click(cx, "g600-profile-3");
    click_track(cx, 400);
    click(cx, "g600-save");
    let (_, _, after) = profile_write_after(&mut receiver);
    assert_eq!(after, StageAfterStore::Keep, "profile 3 is not in use");
    cx.update(|_, cx| {
        AppState::apply(cx, |state| state.apply_g600_dpi_written(&key, Ok(())));
    });
    cx.run_until_parked();

    assert!(writes(&mut receiver).is_empty());
}

#[gpui::test]
fn the_stage_in_use_is_marked_on_its_profile_until_it_is_dragged(cx: &mut gpui::TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (_receiver, cx) = open_card(cx);
    let marked = |cx: &mut VisualTestContext| {
        [0, 1].map(|index| {
            cx.debug_bounds(Box::leak(
                format!("g600-dpi-thumb-{index}-mark").into_boxed_str(),
            ))
            .is_some()
        })
    };

    assert_eq!(marked(cx), [true, false], "the mouse uses 800 on profile 1");
    assert!(cx.debug_bounds("g600-legend-in-use").is_some());

    click(cx, "g600-profile-3");
    assert_eq!(marked(cx), [false, false], "profile 3 is not in use");
    assert!(cx.debug_bounds("g600-legend-in-use").is_none());

    // Dragged off 800, and another stage dragged onto it: neither is the
    // stage the mouse is using.
    click(cx, "g600-profile-1");
    let from = dot(cx, 0);
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    for target in [1000, 1200] {
        let at = over(cx, target);
        cx.simulate_mouse_move(at, MouseButton::Left, Modifiers::none());
    }
    let at = over(cx, 1200);
    cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
    redraw(cx);
    assert_eq!(marked(cx), [false, false]);
    click_track(cx, 800);
    assert_eq!(
        marked(cx),
        [false, false],
        "a new stage at 800 is not the one in use"
    );
}

#[gpui::test]
fn a_typed_dpi_lands_on_its_stage_when_focus_moves_on(cx: &mut gpui::TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (mut receiver, cx) = open_card(cx);
    // Focus listeners, the field's commit on blur among them, run only in
    // the active window.
    cx.update(|window, _| window.activate_window());
    redraw(cx);

    click_dot(cx, 1);
    click(cx, "g600-selected-dpi");
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("1200");
    // Clicking the 800 dot takes focus from the field before it commits.
    click_dot(cx, 0);
    redraw(cx);
    click(cx, "g600-save");

    assert_eq!(
        profile_write(&mut receiver).1,
        settings(&[800, 1200], 1),
        "1200 went to the 1600 stage it was typed for"
    );
}

#[gpui::test]
fn a_press_on_a_dots_label_selects_it_and_adds_nothing(cx: &mut gpui::TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    let (mut receiver, cx) = open_card(cx);
    let rail = cx
        .debug_bounds("g600-dpi-rail")
        .expect("the card shows the DPI track");

    // The label "1600" over its dot, a little off its centre.
    let label = point(dot(cx, 1).x + px(6.), rail.top() + px(9.));
    cx.simulate_click(label, Modifiers::none());
    redraw(cx);
    // The ruler label "3200" under the rail.
    let ruler = point(over(cx, 3200).x, rail.top() + px(64.));
    cx.simulate_click(ruler, Modifiers::none());
    redraw(cx);
    click(cx, "g600-set-default");
    click(cx, "g600-save");

    assert_eq!(
        profile_write(&mut receiver).1,
        settings(&[800, 1600], 2),
        "the label selected 1600, and nothing was added"
    );
}

#[test]
fn a_stage_can_be_made_default_or_removed_only_when_that_changes_something() {
    use super::StageCommands;
    use super::draft::G600Draft;

    let one = G600Draft::from_profile(&ProfileDpi {
        stages: [Some(800), None, None, None],
        default_stage: Some(stage(1)),
        shift: None,
    });
    let two = G600Draft::from_profile(&reading().profiles[0]);

    let commands = |draft: &G600Draft, index| {
        let StageCommands {
            can_default,
            removable,
        } = StageCommands::of(draft, index);
        (can_default, removable)
    };
    assert_eq!(commands(&one, 0), (false, false), "the only stage");
    assert_eq!(commands(&two, 0), (false, true), "the default of two");
    assert_eq!(commands(&two, 1), (true, true));
    assert_eq!(commands(&two, 2), (false, false), "no such stage");
}
