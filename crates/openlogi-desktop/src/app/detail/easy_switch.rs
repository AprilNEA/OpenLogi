//! The Device tab's Easy-Switch card: the computers a multi-host device keeps
//! on its channels (HID++ `0x1815`), as Logi Options+ shows them — the
//! channel, the computer's name and OS, how the channel connects — plus which
//! one is this computer, with a rename for devices that allow it.
//!
//! Nothing is stored on this computer: the device keeps each channel's name
//! and OS in its own memory, so every row shows its last reading. A rename
//! goes through [`AppState::rename_easy_switch_host`], which shows the name
//! at once and lets the device's read-back replace it.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, Icon, IconName, WindowExt as _, button::Button, h_flex, input::InputState,
    v_flex,
};
use openlogi_core::hid::{EasySwitchHosts, EasySwitchSlot, HostConnection, WriteError};

use crate::state::{AppState, DeviceKey, EasySwitchLoad, HostRenameRefusal};
use crate::ui::components::{PanelCard, control_input, save_cancel_footer};
use crate::ui::status::{retry_line, status_line};
use crate::ui::theme::{self, Palette, Typography as _};

/// What the card needs about the selected device.
pub(super) struct EasySwitchFacts {
    key: DeviceKey,
    load: EasySwitchLoad,
    reachable: bool,
    refusal: Option<HostRenameRefusal>,
}

impl EasySwitchFacts {
    /// The selected device's Easy-Switch facts, or `None` when nothing is
    /// selected or the device reports no Easy-Switch hosts.
    pub(super) fn current(cx: &App) -> Option<Self> {
        let state = AppState::try_read(cx)?;
        if !state.current_easy_switch_supported() {
            return None;
        }
        let record = state.current_record()?;
        Some(Self {
            key: record.device_key(),
            load: state.current_easy_switch_load(),
            reachable: state.current_easy_switch_reachable(),
            refusal: state.current_host_rename_refusal(),
        })
    }
}

/// The card, or `None` to hide it: no hosts feature, a device that turned out
/// not to have one or reports no channels, or an offline device with no
/// reading to show (the details card already says it is offline).
pub(super) fn easy_switch_card(facts: Option<EasySwitchFacts>, pal: Palette) -> Option<AnyElement> {
    let EasySwitchFacts {
        key,
        load,
        reachable,
        refusal,
    } = facts?;
    let body: AnyElement = match load {
        EasySwitchLoad::Ready(hosts) if hosts.slots.is_empty() => return None,
        EasySwitchLoad::Ready(hosts) => {
            host_rows(&key, &hosts, reachable, refusal.as_ref(), pal).into_any_element()
        }
        EasySwitchLoad::Unknown | EasySwitchLoad::Loading if !reachable => return None,
        EasySwitchLoad::Unknown | EasySwitchLoad::Loading => {
            status_line(tr!("device.reading_easy_switch"), pal).into_any_element()
        }
        EasySwitchLoad::Failed(_) => retry_line(
            "easy-switch-retry",
            tr!("device.couldnt_read_easy_switch_click_to_retry"),
            pal,
            move |cx| AppState::apply(cx, |state| state.retry_easy_switch_read(&key)),
        )
        .into_any_element(),
        EasySwitchLoad::Unsupported(_) => return None,
    };
    Some(
        PanelCard::new(
            tr!("device.easy_switch"),
            Icon::new(IconName::Network),
            body,
        )
        .into_any_element(),
    )
}

/// One row per channel, in the order the device's keys number them, then why
/// the last rename did not land.
fn host_rows(
    key: &DeviceKey,
    hosts: &Arc<EasySwitchHosts>,
    reachable: bool,
    refusal: Option<&HostRenameRefusal>,
    pal: Palette,
) -> impl IntoElement {
    v_flex()
        .gap_4()
        .children(
            hosts
                .slots
                .iter()
                .map(|slot| host_row(key, hosts, slot, reachable, pal)),
        )
        .children(refusal.map(|refusal| {
            div()
                .debug_selector(|| "easy-switch-refusal".into())
                .text_caption()
                .text_color(pal.text_muted)
                .child(refusal_text(refusal))
        }))
}

/// A channel: the computer's name over its details on the left, the rename
/// button on the right. The text column shrinks and wraps, so a long name or
/// translation cannot push the button out of the card.
fn host_row(
    key: &DeviceKey,
    hosts: &Arc<EasySwitchHosts>,
    slot: &EasySwitchSlot,
    reachable: bool,
    pal: Palette,
) -> impl IntoElement {
    let index = usize::from(slot.index);
    let renamable = hosts.can_rename && slot.paired;
    h_flex()
        .id(("easy-switch-slot", index))
        .debug_selector(move || format!("easy-switch-slot-{index}"))
        .w_full()
        .min_w_0()
        .gap_3()
        .items_center()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .text_body()
                        .text_color(if slot.paired {
                            pal.text_primary
                        } else {
                            pal.text_muted
                        })
                        .child(host_title(slot)),
                )
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(host_details(hosts, slot)),
                ),
        )
        .when(renamable, |row| {
            let key = key.clone();
            let slot = slot.clone();
            row.child(
                div()
                    .debug_selector(move || format!("easy-switch-slot-{index}-rename"))
                    .flex_none()
                    .child(
                        Button::new(("easy-switch-rename", index))
                            .compact()
                            .label(tr!("common.rename_dialog"))
                            .disabled(!reachable)
                            .on_click(move |_event, window, cx| {
                                open_rename_dialog(window, cx, key.clone(), &slot);
                            }),
                    ),
            )
        })
}

/// The computer's name, or what stands in for it.
fn host_title(slot: &EasySwitchSlot) -> SharedString {
    match (&slot.name, slot.paired) {
        (_, false) => tr!("device.no_computer_paired"),
        (Some(name), true) => name.clone().into(),
        (None, true) => tr!("device.unnamed_computer"),
    }
}

/// "Channel 2 · This computer · macOS 15.1 · Bluetooth": the channel, then
/// whatever the device reports of the computer on it.
fn host_details(hosts: &EasySwitchHosts, slot: &EasySwitchSlot) -> String {
    let mut parts: Vec<String> =
        vec![tr!("device.easy_switch_channel", channel => slot.index + 1).to_string()];
    if hosts.current == Some(slot.index) {
        parts.push(tr!("device.this_computer").to_string());
    }
    if slot.paired {
        parts.extend(slot.os.map(|os| os.to_string()));
        parts.extend(connection_label(slot.connection).map(|label| label.to_string()));
    }
    parts.join(" · ")
}

fn connection_label(connection: HostConnection) -> Option<SharedString> {
    match connection {
        HostConnection::Unknown => None,
        HostConnection::Bluetooth => Some(tr!("device.bluetooth")),
        HostConnection::Bolt => Some(tr!("device.bolt_receiver")),
        HostConnection::Unifying => Some(tr!("device.unifying_receiver")),
        HostConnection::Usb => Some(tr!("device.usb")),
    }
}

fn refusal_text(refusal: &HostRenameRefusal) -> SharedString {
    let channel = refusal.slot + 1;
    match refusal.error {
        WriteError::HostNameNotApplied { .. } => {
            tr!("device.host_rename_not_kept", channel => channel)
        }
        _ => tr!("device.host_rename_failed", channel => channel),
    }
}

/// Whether a typed name fits the channel, by the rules
/// [`EasySwitchHosts::name_for`] applies: trimmed, not empty, at most the
/// channel's byte length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameCheck {
    Fits { used: usize },
    Empty,
    TooLong { used: usize },
}

fn name_check(value: &str, max: usize) -> NameCheck {
    let name = value.trim();
    match name.len() {
        0 => NameCheck::Empty,
        used if used > max => NameCheck::TooLong { used },
        used => NameCheck::Fits { used },
    }
}

/// The caption under the name field: how much of the channel's room the name
/// takes, that it is too long once it does not fit, and a prompt for a name
/// once Save was pressed on an empty field.
fn name_caption(check: NameCheck, max: usize, tried: bool) -> SharedString {
    match check {
        NameCheck::Fits { used } => tr!("device.host_name_length", used => used, max => max),
        NameCheck::TooLong { used } => tr!("device.host_name_too_long", used => used, max => max),
        NameCheck::Empty if tried => tr!("device.host_name_empty"),
        NameCheck::Empty => tr!("device.host_name_length", used => 0, max => max),
    }
}

fn open_rename_dialog(window: &mut Window, cx: &mut App, key: DeviceKey, slot: &EasySwitchSlot) {
    let channel = slot.index + 1;
    let index = slot.index;
    let max = usize::from(slot.name_max_len);
    let input = cx.new(|cx| {
        let mut input = InputState::new(window, cx).placeholder(tr!("device.computer_name"));
        if let Some(name) = &slot.name {
            input.set_value(name.clone(), window, cx);
        }
        input
    });
    // Set once Save is pressed on a name that does not fit, so an empty field
    // says why Save did nothing.
    let tried = Rc::new(Cell::new(false));
    window.open_dialog(cx, move |dialog, window, cx| {
        input.update(cx, |input, cx| input.focus(window, cx));
        let pal = theme::palette(cx);
        let check = name_check(&typed(&input, cx), max);
        let caption = name_caption(check, max, tried.get());
        dialog
            .w(px(420.))
            .title(tr!("device.rename_computer_on_channel", channel => channel))
            .child(
                v_flex().gap_2().child(control_input(&input)).child(
                    div()
                        .debug_selector(|| "host-name-caption".into())
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(caption),
                ),
            )
            .footer(save_cancel_footer())
            .on_ok({
                let input = input.clone();
                let key = key.clone();
                let tried = tried.clone();
                move |_, window, cx| {
                    let name = typed(&input, cx);
                    if !matches!(name_check(&name, max), NameCheck::Fits { .. }) {
                        tried.set(true);
                        window.refresh();
                        return false;
                    }
                    AppState::apply(cx, |state| {
                        state.rename_easy_switch_host(&key, index, &name)
                    });
                    true
                }
            })
    });
}

/// The field's text without NULs, which a device name cannot hold and no
/// keyboard types.
fn typed(input: &gpui::Entity<InputState>, cx: &App) -> String {
    without_nul(&input.read(cx).value())
}

/// `value` without NULs: a pasted NUL is dropped rather than refused, so the
/// name the dialog checks is the name it sends.
fn without_nul(value: &str) -> String {
    value.replace('\0', "")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{
        AppContext as _, Context, IntoElement, Modifiers, ParentElement as _, Render, Styled as _,
        TestAppContext, VisualTestContext, Window, div, px, size,
    };
    use openlogi_core::config::Config;
    use openlogi_core::hid::{
        EasySwitchHosts, EasySwitchSlot, HostConnection, HostOsInfo, HostOsKind, WriteError,
    };

    use super::{
        EasySwitchFacts, NameCheck, easy_switch_card, host_details, host_title, name_check,
        open_rename_dialog, refusal_text, without_nul,
    };
    use crate::services::assets::AssetResolver;
    use crate::services::i18n::LOCALE_LOCK;
    use crate::state::{AppState, DeviceKey, HostRenameRefusal, Load, Sources};
    use crate::ui::theme;

    const ROWS: [&str; 3] = [
        "easy-switch-slot-0",
        "easy-switch-slot-1",
        "easy-switch-slot-2",
    ];
    const RENAMES: [&str; 3] = [
        "easy-switch-slot-0-rename",
        "easy-switch-slot-1-rename",
        "easy-switch-slot-2-rename",
    ];

    fn slot(index: u8, name: Option<&str>) -> EasySwitchSlot {
        EasySwitchSlot {
            index,
            paired: name.is_some(),
            connection: if name.is_some() {
                HostConnection::Bluetooth
            } else {
                HostConnection::Unknown
            },
            name: name.map(str::to_owned),
            name_max_len: 24,
            os: name.map(|_| HostOsInfo {
                kind: HostOsKind::MacOs,
                major: 15,
                minor: 1,
                patch: 0,
            }),
        }
    }

    fn hosts(can_rename: bool) -> EasySwitchHosts {
        EasySwitchHosts {
            current: Some(0),
            slots: vec![
                slot(0, Some("A rather long workstation name")),
                slot(1, Some("Studio")),
                slot(2, None),
            ],
            can_rename,
        }
    }

    /// Hosts the card at a narrow card width so long names have to wrap.
    struct CardHost(EasySwitchHosts, Option<HostRenameRefusal>);

    impl Render for CardHost {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let facts = EasySwitchFacts {
                key: DeviceKey::from("direct:046d:b35b:unit:a393cae0"),
                load: Load::Ready(Arc::new(self.0.clone())),
                reachable: true,
                refusal: self.1.clone(),
            };
            div()
                .w(px(360.))
                .children(easy_switch_card(Some(facts), theme::palette(cx)))
        }
    }

    fn draw(
        cx: &mut TestAppContext,
        hosts: EasySwitchHosts,
        refusal: Option<HostRenameRefusal>,
    ) -> &mut gpui::VisualTestContext {
        cx.update(gpui_component::init);
        let (_view, cx) = cx.add_window_view(|_, _| CardHost(hosts, refusal));
        cx.simulate_resize(size(px(800.), px(600.)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx
    }

    #[gpui::test]
    fn every_channel_gets_a_row_and_paired_ones_keep_their_rename_inside(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("fi");
        let cx = draw(cx, hosts(true), None);

        for row in ROWS {
            let bounds = cx
                .debug_bounds(row)
                .unwrap_or_else(|| panic!("{row} renders"));
            assert!(
                bounds.size.width <= px(360.),
                "{row} {bounds:?} must fit its card"
            );
        }
        for (row, rename) in ROWS.into_iter().zip(RENAMES).take(2) {
            let bounds = cx.debug_bounds(row).expect("row");
            let button = cx
                .debug_bounds(rename)
                .unwrap_or_else(|| panic!("{row} offers a rename"));
            assert!(
                button.left() >= bounds.left() && button.right() <= bounds.right(),
                "{row}'s rename {button:?} must sit inside its row {bounds:?}"
            );
        }
        assert!(
            cx.debug_bounds("easy-switch-slot-2-rename").is_none(),
            "an empty channel has no computer to rename"
        );
        assert!(cx.debug_bounds("easy-switch-refusal").is_none());
    }

    #[gpui::test]
    fn a_device_that_cannot_rename_offers_no_rename(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let cx = draw(cx, hosts(false), None);

        assert!(cx.debug_bounds("easy-switch-slot-0").is_some());
        for rename in RENAMES {
            assert!(cx.debug_bounds(rename).is_none(), "{rename} is not offered");
        }
    }

    #[gpui::test]
    fn a_refused_rename_is_explained_under_the_channels(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let refusal = HostRenameRefusal {
            slot: 1,
            error: WriteError::AgentUnavailable,
        };
        let cx = draw(cx, hosts(true), Some(refusal));

        assert!(cx.debug_bounds("easy-switch-refusal").is_some());
    }

    /// Paints the dialog layer the way the app's root view does.
    struct DialogHost;

    impl Render for DialogHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .children(gpui_component::Root::render_dialog_layer(window, cx))
        }
    }

    /// The card of a device that is `reachable` or not, over the dialog layer,
    /// so its renames can open their dialog.
    struct RootedCard {
        reachable: bool,
    }

    impl Render for RootedCard {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let facts = EasySwitchFacts {
                key: DeviceKey::from("direct:046d:b35b:unit:a393cae0"),
                load: Load::Ready(Arc::new(hosts(true))),
                reachable: self.reachable,
                refusal: None,
            };
            div()
                .size_full()
                .children(easy_switch_card(Some(facts), theme::palette(cx)))
                .children(gpui_component::Root::render_dialog_layer(window, cx))
        }
    }

    fn dialog_window(cx: &mut TestAppContext) -> &mut VisualTestContext {
        rooted_window(cx, || DialogHost)
    }

    /// A window whose root view is `view`, as the app's root wraps its own.
    ///
    /// Motion is reduced: a dialog slides in on the wall clock, so on a slow
    /// runner a redraw between measuring a button and clicking it would move
    /// the button away from the click. Its resting position is what is tested.
    fn rooted_window<V: Render>(
        cx: &mut TestAppContext,
        view: impl FnOnce() -> V + 'static,
    ) -> &mut VisualTestContext {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_reduce_motion(true);
            let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
            let resolver = AssetResolver::new();
            let state = cx.new(|_| {
                AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands))
            });
            AppState::set_global(state, cx);
        });
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| view());
            gpui_component::Root::new(view, window, cx)
        });
        cx.simulate_resize(size(px(800.), px(600.)));
        cx
    }

    fn redraw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn press(cx: &mut VisualTestContext, button: &'static str) {
        let bounds = cx
            .debug_bounds(button)
            .unwrap_or_else(|| panic!("{button} renders"));
        cx.simulate_click(bounds.center(), Modifiers::none());
        redraw(cx);
    }

    #[gpui::test]
    fn the_rename_dialog_saves_only_a_name_that_fits_and_cancels(cx: &mut TestAppContext) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let cx = dialog_window(cx);
        let key = DeviceKey::from("direct:046d:b35b:unit:a393cae0");
        let unnamed = EasySwitchSlot {
            name: None,
            ..slot(1, Some("Studio"))
        };
        cx.update(|window, cx| open_rename_dialog(window, cx, key.clone(), &unnamed));
        redraw(cx);
        assert!(
            cx.debug_bounds("dialog-cancel").is_some(),
            "the dialog offers a way out besides Escape"
        );

        press(cx, "dialog-save");
        assert!(
            cx.debug_bounds("host-name-caption").is_some(),
            "Save on an empty field keeps the dialog open"
        );
        cx.simulate_input("Desk");
        redraw(cx);
        press(cx, "dialog-save");
        assert!(
            cx.debug_bounds("host-name-caption").is_none(),
            "Save on a name that fits closes the dialog"
        );

        cx.update(|window, cx| open_rename_dialog(window, cx, key.clone(), &unnamed));
        redraw(cx);
        press(cx, "dialog-cancel");
        assert!(
            cx.debug_bounds("host-name-caption").is_none(),
            "Cancel closes the dialog"
        );
    }

    /// Whether pressing channel 1's rename on a device that is `reachable` or
    /// not opens the rename dialog.
    fn rename_opens(cx: &mut TestAppContext, reachable: bool) -> bool {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let cx = rooted_window(cx, move || RootedCard { reachable });
        redraw(cx);
        press(cx, RENAMES[0]);
        cx.debug_bounds("host-name-caption").is_some()
    }

    #[gpui::test]
    fn a_reachable_device_s_rename_opens_its_dialog(cx: &mut TestAppContext) {
        assert!(rename_opens(cx, true));
    }

    /// An offline device keeps its rows from the last reading, with the
    /// rename there but disabled.
    #[gpui::test]
    fn an_offline_device_s_rename_is_disabled(cx: &mut TestAppContext) {
        assert!(!rename_opens(cx, false));
    }

    #[gpui::test]
    fn the_card_is_hidden_without_channels_a_hosts_feature_or_a_reading_to_show(
        cx: &mut TestAppContext,
    ) {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let pal = theme::palette(cx);
            let card = |load, reachable| {
                easy_switch_card(
                    Some(EasySwitchFacts {
                        key: DeviceKey::from("direct:046d:b35b:unit:a393cae0"),
                        load,
                        reachable,
                        refusal: None,
                    }),
                    pal,
                )
                .is_some()
            };
            let no_channels = EasySwitchHosts {
                current: None,
                slots: Vec::new(),
                can_rename: true,
            };

            assert!(easy_switch_card(None, pal).is_none(), "no hosts feature");
            assert!(
                !card(Load::Ready(Arc::new(no_channels)), true),
                "no channels"
            );
            assert!(
                !card(Load::Unsupported("0x1815".into()), true),
                "the read found no hosts feature"
            );
            for load in [Load::Unknown, Load::Loading] {
                assert!(
                    !card(load.clone(), false),
                    "offline, nothing read: {load:?}"
                );
                assert!(card(load.clone(), true), "being read: {load:?}");
            }
            assert!(
                card(Load::Ready(Arc::new(hosts(true))), false),
                "an offline device keeps its last reading"
            );
            assert!(
                card(Load::Failed("timeout".into()), true),
                "a failed read offers a retry"
            );
        });
    }

    #[test]
    fn a_pasted_nul_is_dropped_from_the_name() {
        assert_eq!(without_nul("Desk\0 Mac\0"), "Desk Mac");
        assert_eq!(without_nul("Studio"), "Studio");
    }

    #[test]
    fn a_row_names_the_channel_this_computer_its_os_and_its_connection() {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let hosts = hosts(true);

        assert_eq!(
            host_details(&hosts, &hosts.slots[0]),
            "Channel 1 · This computer · macOS 15.1 · Bluetooth"
        );
        assert_eq!(
            host_details(&hosts, &hosts.slots[1]),
            "Channel 2 · macOS 15.1 · Bluetooth"
        );
        // An empty channel reports nothing about a computer.
        assert_eq!(host_details(&hosts, &hosts.slots[2]), "Channel 3");
        assert_eq!(host_title(&hosts.slots[2]), "No computer paired");
        let unnamed = EasySwitchSlot {
            name: None,
            ..hosts.slots[1].clone()
        };
        assert_eq!(host_title(&unnamed), "Unnamed computer");
    }

    #[test]
    fn a_name_fits_once_trimmed_it_is_not_empty_and_within_the_channels_bytes() {
        assert_eq!(name_check("  Studio ", 6), NameCheck::Fits { used: 6 });
        assert_eq!(name_check("   ", 6), NameCheck::Empty);
        assert_eq!(name_check("Studios", 6), NameCheck::TooLong { used: 7 });
        // Bytes, not characters: "é" takes two.
        assert_eq!(name_check("Caféé", 6), NameCheck::TooLong { used: 7 });
    }

    #[test]
    fn a_rename_the_device_did_not_keep_says_so() {
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let not_kept = HostRenameRefusal {
            slot: 1,
            error: WriteError::HostNameNotApplied {
                requested: "Studio".to_owned(),
                actual: Some("Old".to_owned()),
            },
        };
        assert_eq!(
            refusal_text(&not_kept),
            "The device didn't keep the new name for channel 2."
        );
        let failed = HostRenameRefusal {
            slot: 0,
            error: WriteError::AgentUnavailable,
        };
        assert_eq!(
            refusal_text(&failed),
            "Couldn't rename the computer on channel 1. Try again."
        );
    }
}
