//! The device card menu's settings transfer: copy settings from another
//! device, import them from a file, export them to a file. Every copy shows
//! what changes and what is left out before anything is written.

use std::path::{Path, PathBuf};

use gpui::{
    App, Context, IntoElement, ParentElement as _, PathPromptOptions, SharedString, Styled as _,
    Window, div, prelude::FluentBuilder as _, px,
};
use gpui_kit::component::{
    Icon, WindowExt as _,
    dialog::DialogButtonProps,
    menu::{PopupMenu, PopupMenuItem},
    v_flex,
};
use openlogi_core::binding::ButtonId;
use openlogi_core::config::{Config, SettingsTransfer, SkipReason, SkippedItem, TransferItem};

use crate::state::{AppState, TransferSource};
use crate::ui::components::control_button;
use crate::ui::theme::{self, Palette, Typography as _};

const COPY_ICON: &str = "action-icons/copy.svg";
const IMPORT_ICON: &str = "action-icons/file.svg";
const EXPORT_ICON: &str = "action-icons/save.svg";

/// Add the transfer items for the card `record_key` to its menu.
pub(super) fn transfer_items(
    menu: PopupMenu,
    record_key: &str,
    display_name: &str,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let sources = AppState::try_read(cx)
        .map(|state| state.transfer_sources(record_key))
        .unwrap_or_default();
    let menu = if sources.is_empty() {
        menu.item(
            PopupMenuItem::new(tr!("device.copy_settings_from"))
                .icon(Icon::empty().path(COPY_ICON))
                .disabled(true),
        )
    } else {
        let record_key = record_key.to_owned();
        menu.submenu_with_icon(
            Some(Icon::empty().path(COPY_ICON)),
            tr!("device.copy_settings_from"),
            window,
            cx,
            move |submenu, _window, _cx| {
                sources.iter().fold(submenu, |submenu, source| {
                    let source = source.clone();
                    let record_key = record_key.clone();
                    submenu.item(PopupMenuItem::new(source.name.clone()).on_click(
                        move |_, window, cx| {
                            open_transfer_preview(window, cx, &record_key, &source);
                        },
                    ))
                })
            },
        )
    };
    let exportable = AppState::try_read(cx)
        .and_then(|state| state.device_export(record_key))
        .is_some();
    menu.item(
        PopupMenuItem::new(tr!("device.import_settings"))
            .icon(Icon::empty().path(IMPORT_ICON))
            .on_click({
                let record_key = record_key.to_owned();
                move |_, window, cx| import_settings(window, cx, record_key.clone())
            }),
    )
    .item(
        PopupMenuItem::new(tr!("device.export_settings"))
            .icon(Icon::empty().path(EXPORT_ICON))
            .disabled(!exportable)
            .on_click({
                let record_key = record_key.to_owned();
                let display_name = display_name.to_owned();
                move |_, window, cx| export_settings(window, cx, &record_key, &display_name)
            }),
    )
}

/// Show what copying `source` onto the card `record_key`'s device changes
/// and leaves out, and copy it on confirmation.
fn open_transfer_preview(
    window: &mut Window,
    cx: &mut App,
    record_key: &str,
    source: &TransferSource,
) {
    let Some((target, transfer)) = AppState::try_read(cx).and_then(|state| {
        let target = state
            .devices()
            .iter()
            .find(|record| record.record_key() == record_key)?
            .display_name
            .clone();
        Some((target, state.settings_transfer_preview(record_key, source)?))
    }) else {
        return;
    };
    let record_key = record_key.to_owned();
    let source = source.clone();
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let alert = alert.width(px(480.)).title(tr!(
            "device.copy_settings_title",
            source => source.name.clone(),
            target => target.clone()
        ));
        if transfer.is_empty() {
            let alert =
                alert.description(tr!("device.copy_settings_nothing", target => target.clone()));
            if transfer.skipped.is_empty() {
                return alert;
            }
            return alert.child(preview(&transfer, theme::palette(cx)));
        }
        alert
            .child(preview(&transfer, theme::palette(cx)))
            .button_props(
                DialogButtonProps::default()
                    .ok_text(tr!("device.copy_settings"))
                    .cancel_text(tr!("common.cancel"))
                    .show_cancel(true),
            )
            .on_ok({
                let record_key = record_key.clone();
                let source = source.clone();
                move |_event, _window, cx| {
                    AppState::apply(cx, |state| {
                        state.commit_settings_transfer(&record_key, &source)
                    });
                    true
                }
            })
    });
}

/// Ask for a settings file, then preview copying a device from it onto the
/// card `record_key`'s device. A file with several such devices asks which.
fn import_settings(window: &mut Window, cx: &mut App, record_key: String) {
    let picked = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(tr!("device.import_settings_prompt")),
    });
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let file = Config::load_from_path(&path);
            cx.update(|window, cx| match file {
                Err(error) => open_message(
                    window,
                    cx,
                    tr!("device.import_settings_failed"),
                    error.to_string().into(),
                ),
                Ok(file) => {
                    let sources = AppState::try_read(cx)
                        .map(|state| state.file_transfer_sources(&record_key, &file))
                        .unwrap_or_default();
                    match sources.as_slice() {
                        [] => open_message(
                            window,
                            cx,
                            tr!("device.import_settings_failed"),
                            tr!("device.import_settings_no_device"),
                        ),
                        [source] => open_transfer_preview(window, cx, &record_key, source),
                        _ => open_source_choice(window, cx, record_key.clone(), sources),
                    }
                }
            })
            .ok();
        })
        .detach();
}

/// Ask which of a file's devices to copy from.
fn open_source_choice(
    window: &mut Window,
    cx: &mut App,
    record_key: String,
    sources: Vec<TransferSource>,
) {
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .w(px(420.))
            .title(tr!("device.import_settings_choose"))
            .child(
                v_flex()
                    .gap_2()
                    .children(sources.iter().enumerate().map(|(index, source)| {
                        let source = source.clone();
                        let record_key = record_key.clone();
                        control_button(("import-settings-source", index))
                            .label(source.name.clone())
                            .w_full()
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                open_transfer_preview(window, cx, &record_key, &source);
                            })
                    })),
            )
    });
}

/// Ask where to save the card `record_key`'s device settings, and save them.
fn export_settings(window: &mut Window, cx: &mut App, record_key: &str, display_name: &str) {
    let Some(export) = AppState::try_read(cx).and_then(|state| state.device_export(record_key))
    else {
        return;
    };
    let file_name = format!("{display_name}.toml");
    let directory = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let picked = cx.prompt_for_new_path(&directory, Some(&file_name));
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(path))) = picked.await else {
                return;
            };
            let written = write_export(&export, &path);
            cx.update(|window, cx| match written {
                Ok(()) => open_message(
                    window,
                    cx,
                    tr!("device.export_settings_done_title"),
                    tr!("device.export_settings_done", path => path.display().to_string()),
                ),
                Err(error) => open_message(
                    window,
                    cx,
                    tr!("device.export_settings_failed"),
                    error.into(),
                ),
            })
            .ok();
        })
        .detach();
}

fn write_export(export: &Config, path: &Path) -> Result<(), String> {
    let text = export.to_toml_string().map_err(|error| error.to_string())?;
    std::fs::write(path, text).map_err(|error| error.to_string())
}

fn open_message(window: &mut Window, cx: &mut App, title: SharedString, text: SharedString) {
    window.open_alert_dialog(cx, move |alert, _, _| {
        alert.title(title.clone()).description(text.clone())
    });
}

fn preview(transfer: &SettingsTransfer, pal: Palette) -> impl IntoElement {
    v_flex()
        .gap_3()
        .when(!transfer.changed.is_empty(), |column| {
            column.child(section(
                tr!("device.copy_settings_changes"),
                transfer.changed.iter().map(item_label),
                pal,
            ))
        })
        .when(!transfer.skipped.is_empty(), |column| {
            column.child(section(
                tr!("device.copy_settings_skipped"),
                transfer.skipped.iter().map(skipped_label),
                pal,
            ))
        })
}

fn section(
    title: SharedString,
    lines: impl Iterator<Item = SharedString>,
    pal: Palette,
) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(div().text_caption().text_color(pal.text_muted).child(title))
        .children(lines.map(|line| div().text_body().child(line)))
}

fn button_label(button: ButtonId) -> SharedString {
    tr!(button.translation_key())
}

fn item_label(item: &TransferItem) -> SharedString {
    match item {
        TransferItem::Button(button) => button_label(*button),
        TransferItem::StoredGestures(button) => {
            tr!("device.copy_settings_stored_gestures", button => button_label(*button))
        }
        TransferItem::AppButton { app, button } => tr!(
            "device.copy_settings_app_button",
            button => button_label(*button),
            app => app.clone()
        ),
        TransferItem::ActionRing => tr!("action_ring.actions_ring"),
        TransferItem::ThumbwheelSensitivity => tr!("pointer.thumb_wheel_sensitivity"),
        TransferItem::InvertScroll => tr!("pointer.invert_scroll_direction"),
        TransferItem::ScrollResolution => tr!("pointer.wheel_resolution"),
        TransferItem::SmoothWheel => tr!("pointer.smooth_wheel"),
        TransferItem::MomentumScrolling => tr!("pointer.momentum_title"),
        TransferItem::SmartShift => tr!("pointer.smartshift"),
        TransferItem::Dpi => tr!("pointer.dpi"),
        TransferItem::DpiPresets => tr!("pointer.dpi_presets"),
        TransferItem::PointerSpeed => tr!("pointer.pointer_speed"),
    }
}

fn skipped_label(skipped: &SkippedItem) -> SharedString {
    let reason = match skipped.reason {
        SkipReason::NoHapticPanel => tr!("device.copy_settings_no_haptic_panel"),
        SkipReason::NoThumbwheel => tr!("device.copy_settings_no_thumbwheel"),
        SkipReason::NoDpiGestures => tr!("device.copy_settings_no_dpi_gestures"),
        SkipReason::NoScrollInversion => tr!("device.copy_settings_no_scroll_inversion"),
        SkipReason::NoHiresWheel => tr!("device.copy_settings_no_hires_wheel"),
        SkipReason::NoDpi => tr!("device.copy_settings_no_dpi"),
        SkipReason::NoPointerSpeed => tr!("device.copy_settings_no_pointer_speed"),
        SkipReason::NotOnAMouse => tr!("device.copy_settings_not_on_a_mouse"),
        SkipReason::RunsCode => tr!("device.copy_settings_runs_code"),
    };
    tr!(
        "device.copy_settings_skipped_item",
        item => item_label(&skipped.item),
        reason => reason
    )
}
