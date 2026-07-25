//! Windows helpers for synthesising OS-level input events via `SendInput`.
#![allow(unsafe_code, reason = "SendInput is the Win32 API for synthetic input")]

use std::mem::size_of;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
    MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SendInput,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;

use openlogi_core::binding::{Action, KeyCombo, split_run_target};

const WHEEL_DELTA: i32 = 120;

const VK_A: u16 = 0x41;
const VK_C: u16 = 0x43;
const VK_D: u16 = 0x44;
const VK_F: u16 = 0x46;
const VK_L: u16 = 0x4C;
const VK_R: u16 = 0x52;
const VK_S: u16 = 0x53;
const VK_T: u16 = 0x54;
const VK_V: u16 = 0x56;
const VK_W: u16 = 0x57;
const VK_X: u16 = 0x58;
const VK_Y: u16 = 0x59;
const VK_Z: u16 = 0x5A;
const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0D;
const VK_LEFT: u16 = 0x25;
const VK_RIGHT: u16 = 0x27;
const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;
const VK_LWIN: u16 = 0x5B;
const VK_BROWSER_BACK: u16 = 0xA6;
const VK_BROWSER_FORWARD: u16 = 0xA7;
const VK_VOLUME_MUTE: u16 = 0xAD;
const VK_VOLUME_DOWN: u16 = 0xAE;
const VK_VOLUME_UP: u16 = 0xAF;
const VK_MEDIA_NEXT_TRACK: u16 = 0xB0;
const VK_MEDIA_PREV_TRACK: u16 = 0xB1;
const VK_MEDIA_PLAY_PAUSE: u16 = 0xB3;

#[derive(Clone, Copy)]
enum MouseButton {
    Left,
    Right,
    Middle,
    /// Extra button 4 ("back").
    Back,
    /// Extra button 5 ("forward").
    Forward,
}

// XBUTTON1/XBUTTON2 from WinUser.h — windows-sys puts them behind the
// Win32_UI_WindowsAndMessaging feature; not worth enabling for two
// integers (same treatment as the VK_* codes above).
const XBUTTON1: i32 = 1;
const XBUTTON2: i32 = 2;

/// Windows implementation: synthesise events via `SendInput`. macOS
/// window-manager actions map to their Windows equivalents; `CustomShortcut`
/// maps macOS `kVK_*` codes to Windows virtual-key codes (Cmd → Ctrl).
pub(super) fn execute(action: &Action) {
    match action {
        Action::LeftClick => post_click(MouseButton::Left),
        Action::RightClick => post_click(MouseButton::Right),
        Action::MiddleClick => post_click(MouseButton::Middle),
        Action::MouseBack => post_click(MouseButton::Back),
        Action::MouseForward => post_click(MouseButton::Forward),
        Action::Copy => post_key(VK_C, &[VK_CONTROL]),
        Action::Paste => post_key(VK_V, &[VK_CONTROL]),
        Action::Cut => post_key(VK_X, &[VK_CONTROL]),
        Action::Undo => post_key(VK_Z, &[VK_CONTROL]),
        Action::Redo => post_key(VK_Y, &[VK_CONTROL]),
        Action::SelectAll => post_key(VK_A, &[VK_CONTROL]),
        Action::Find => post_key(VK_F, &[VK_CONTROL]),
        Action::Save => post_key(VK_S, &[VK_CONTROL]),
        Action::BrowserBack => post_key(VK_BROWSER_BACK, &[]),
        Action::BrowserForward => post_key(VK_BROWSER_FORWARD, &[]),
        Action::NewTab => post_key(VK_T, &[VK_CONTROL]),
        Action::CloseTab => post_key(VK_W, &[VK_CONTROL]),
        Action::ReopenTab => {
            post_key(VK_T, &[VK_CONTROL, VK_SHIFT]);
        }
        Action::NextTab => post_key(VK_TAB, &[VK_CONTROL]),
        Action::PrevTab => {
            post_key(VK_TAB, &[VK_CONTROL, VK_SHIFT]);
        }
        Action::ReloadPage => post_key(VK_R, &[VK_CONTROL]),
        Action::MissionControl | Action::AppExpose => {
            post_key(VK_TAB, &[VK_LWIN]);
        }
        Action::PreviousDesktop => {
            post_key(VK_LEFT, &[VK_LWIN, VK_CONTROL]);
        }
        Action::NextDesktop => {
            post_key(VK_RIGHT, &[VK_LWIN, VK_CONTROL]);
        }
        Action::ShowDesktop => post_key(VK_D, &[VK_LWIN]),
        Action::LaunchpadShow => post_key(VK_LWIN, &[]),
        Action::LockScreen => post_key(VK_L, &[VK_LWIN]),
        // Win+Shift+S opens the snip overlay, which serves both full-screen
        // and region capture on Windows.
        Action::Screenshot | Action::CaptureRegion => {
            post_key(VK_S, &[VK_LWIN, VK_SHIFT]);
        }
        Action::PlayPause => post_key(VK_MEDIA_PLAY_PAUSE, &[]),
        Action::NextTrack => post_key(VK_MEDIA_NEXT_TRACK, &[]),
        Action::PrevTrack => post_key(VK_MEDIA_PREV_TRACK, &[]),
        Action::VolumeUp => post_key(VK_VOLUME_UP, &[]),
        Action::VolumeDown => post_key(VK_VOLUME_DOWN, &[]),
        Action::MuteVolume => post_key(VK_VOLUME_MUTE, &[]),
        Action::CycleDpiPresets | Action::SetDpiPreset(_) | Action::ToggleSmartShift => {
            tracing::debug!(
                action = action.label(),
                "device action handled by hook/HID layer"
            );
        }
        Action::ScrollUp
        | Action::ScrollDown
        | Action::HorizontalScrollLeft
        | Action::HorizontalScrollRight => post_scroll(action),
        Action::CustomShortcut(combo) => post_custom_shortcut(combo),
        Action::Run(payload) => run_target(payload),
        Action::PasteText(text) => post_text(text),
        Action::Folder(_) => {
            tracing::warn!("folder reached the injector — containers are resolved by the ring");
        }
        Action::None => {}
    }
}

// SW_SHOWNORMAL from WinUser.h — windows-sys puts it behind the
// Win32_UI_WindowsAndMessaging feature; not worth enabling for one integer
// (same treatment as the VK_* codes above).
const SW_SHOWNORMAL: i32 = 1;

/// Open a [`Action::Run`] target through the shell: URLs launch the default
/// browser, documents open their association, executables run with the `||`
/// argument string. `%VAR%` environment references expand in both halves.
fn run_target(payload: &str) {
    let (target, args) = split_run_target(payload);
    if target.is_empty() {
        tracing::warn!("Run action with an empty target; ignored");
        return;
    }
    let target = expand_env(target);
    let params = args.map(expand_env);

    let wide_target = wide(&target);
    let wide_params = params.as_deref().map(wide);
    let wide_verb = wide("open");
    // SAFETY: all pointers are NUL-terminated UTF-16 buffers that outlive the
    // call; ShellExecuteW copies what it needs before returning.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            wide_verb.as_ptr(),
            wide_target.as_ptr(),
            wide_params.as_ref().map_or(std::ptr::null(), Vec::as_ptr),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW's documented error contract: values ≤ 32 are error codes.
    if result as usize <= 32 {
        tracing::warn!(target = %target, code = result as usize, "Run target failed to open");
    } else {
        tracing::debug!(target = %target, "Run target opened");
    }
}

/// NUL-terminated UTF-16 for Win32 `W` APIs.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Expand `%NAME%` environment references: `%%` escapes a literal `%`, and
/// unknown names keep their literal spelling (cmd.exe behavior).
fn expand_env(value: &str) -> String {
    expand_env_with(value, |name| std::env::var(name).ok())
}

/// [`expand_env`] with the variable source injected, so the parse is
/// deterministic under test.
fn expand_env_with(value: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('%') {
            let name = &after[..end];
            if name.is_empty() {
                out.push('%');
            } else if let Some(resolved) = lookup(name) {
                out.push_str(&resolved);
            } else {
                out.push('%');
                out.push_str(name);
                out.push('%');
            }
            rest = &after[end + 1..];
        } else {
            // A lone trailing % is literal.
            out.push('%');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Type [`Action::PasteText`]'s snippet as `KEYEVENTF_UNICODE` events — no
/// clipboard involvement, so the user's clipboard survives. Line breaks
/// become Enter and tabs become Tab presses so multi-line snippets land the
/// way they read; astral-plane characters ride as surrogate halves, which is
/// the documented `KEYEVENTF_UNICODE` convention.
fn post_text(text: &str) {
    let mut inputs = Vec::with_capacity(text.len() * 2);
    for ch in text.replace("\r\n", "\n").chars() {
        match ch {
            '\n' => {
                inputs.push(key_input(VK_RETURN, false));
                inputs.push(key_input(VK_RETURN, true));
            }
            '\t' => {
                inputs.push(key_input(VK_TAB, false));
                inputs.push(key_input(VK_TAB, true));
            }
            _ => {
                let mut units = [0u16; 2];
                for unit in ch.encode_utf16(&mut units) {
                    inputs.push(unicode_input(*unit, false));
                    inputs.push(unicode_input(*unit, true));
                }
            }
        }
    }
    send_inputs(&inputs);
}

fn unicode_input(unit: u16, key_up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: unit,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn post_click(button: MouseButton) {
    let (down, up, data) = match button {
        MouseButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
        MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
        MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
        // Extra buttons share the X flag pair; mouseData carries which one.
        MouseButton::Back => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, XBUTTON1),
        MouseButton::Forward => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, XBUTTON2),
    };
    send_inputs(&[mouse_input(down, data), mouse_input(up, data)]);
}

fn post_key(vk: u16, modifiers: &[u16]) {
    let mut inputs = Vec::with_capacity(modifiers.len() * 2 + 2);
    for modifier in modifiers {
        inputs.push(key_input(*modifier, false));
    }
    inputs.push(key_input(vk, false));
    inputs.push(key_input(vk, true));
    for modifier in modifiers.iter().rev() {
        inputs.push(key_input(*modifier, true));
    }
    send_inputs(&inputs);
}

fn post_scroll(action: &Action) {
    let (flags, data) = match action {
        Action::ScrollUp => (MOUSEEVENTF_WHEEL, WHEEL_DELTA),
        Action::ScrollDown => (MOUSEEVENTF_WHEEL, -WHEEL_DELTA),
        Action::HorizontalScrollLeft => (MOUSEEVENTF_HWHEEL, -WHEEL_DELTA),
        Action::HorizontalScrollRight => (MOUSEEVENTF_HWHEEL, WHEEL_DELTA),
        _ => return,
    };
    send_inputs(&[mouse_input(flags, data)]);
}

pub(super) fn post_horizontal_scroll(delta: i32) {
    if delta == 0 {
        return;
    }
    send_inputs(&[mouse_input(
        MOUSEEVENTF_HWHEEL,
        delta.saturating_mul(WHEEL_DELTA),
    )]);
}

fn post_custom_shortcut(combo: &KeyCombo) {
    if combo.key_code == 0 {
        tracing::warn!(
            chord = %combo.rendered_label(),
            "CustomShortcut with no key code; press ignored"
        );
        return;
    }
    let Some(vk) = super::mac_virtual_key_to_windows(combo.key_code) else {
        tracing::warn!(
            key_code = combo.key_code,
            chord = %combo.rendered_label(),
            "CustomShortcut key has no Windows mapping yet; press ignored"
        );
        return;
    };

    let mut modifiers = Vec::new();
    if combo.modifiers & KeyCombo::MOD_CMD != 0 {
        modifiers.push(VK_CONTROL);
    }
    if combo.modifiers & KeyCombo::MOD_SHIFT != 0 {
        modifiers.push(VK_SHIFT);
    }
    if combo.modifiers & KeyCombo::MOD_CTRL != 0 && !modifiers.contains(&VK_CONTROL) {
        modifiers.push(VK_CONTROL);
    }
    if combo.modifiers & KeyCombo::MOD_OPTION != 0 {
        modifiers.push(VK_MENU);
    }
    if combo.modifiers & KeyCombo::MOD_WIN != 0 {
        modifiers.push(VK_LWIN);
    }
    post_key(vk, &modifiers);
}

fn send_inputs(inputs: &[INPUT]) {
    let Ok(input_count) = u32::try_from(inputs.len()) else {
        tracing::warn!(
            requested = inputs.len(),
            "too many SendInput events requested"
        );
        return;
    };
    let Ok(input_size) = i32::try_from(size_of::<INPUT>()) else {
        tracing::warn!("INPUT size does not fit the Win32 SendInput contract");
        return;
    };
    // SAFETY: inputs.as_ptr()/input_count describe a valid initialized INPUT slice; SendInput copies it and returns the count injected.
    let sent = unsafe { SendInput(input_count, inputs.as_ptr(), input_size) };
    if sent != input_count {
        tracing::warn!(
            requested = inputs.len(),
            sent,
            "SendInput accepted fewer events than requested"
        );
    }
}

fn key_input(vk: u16, key_up: bool) -> INPUT {
    let mut flags = 0;
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_input(flags: u32, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: u32::from_ne_bytes(data.to_ne_bytes()),
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::expand_env_with;

    #[test]
    fn expand_env_substitutes_known_and_keeps_unknown_names() {
        let lookup = |name: &str| match name {
            "USERPROFILE" => Some(r"C:\Users\demo".to_string()),
            _ => None,
        };
        assert_eq!(
            expand_env_with(r"%USERPROFILE%\notes.txt", lookup),
            r"C:\Users\demo\notes.txt"
        );
        // Unknown names keep their literal spelling, cmd.exe style.
        assert_eq!(expand_env_with(r"%UNSET%\x", lookup), r"%UNSET%\x");
    }

    #[test]
    fn expand_env_treats_doubled_and_lone_percent_as_literals() {
        let lookup = |_: &str| None;
        assert_eq!(expand_env_with("100%% done", lookup), "100% done");
        assert_eq!(expand_env_with("50% off", lookup), "50% off");
        assert_eq!(expand_env_with("", lookup), "");
    }
}
