//! Windows helpers for synthesising OS-level input events via `SendInput`.
#![expect(unsafe_code, reason = "SendInput is the Win32 API for synthetic input")]

use std::mem::size_of;
use std::sync::{LazyLock, Mutex};

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, SendInput,
};

use openlogi_core::binding::{
    Action, Effect, KeyCombo, MediaKey, MouseButton, NativeAction, Script, Shortcut, WorkflowStep,
};
use openlogi_core::scroll::ScrollDelta;

use super::{HeldKey, KeyPhase, ScrollQuantizer};

const WHEEL_DELTA: i32 = 120;
const WHEEL_DELTA_F64: f64 = 120.0;

static SCROLL_QUANTIZER: LazyLock<Mutex<ScrollQuantizer>> =
    LazyLock::new(|| Mutex::new(ScrollQuantizer::default()));

const VK_D: u16 = 0x44;
const VK_L: u16 = 0x4C;
const VK_S: u16 = 0x53;
const VK_TAB: u16 = 0x09;
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

// XBUTTON1/XBUTTON2 from WinUser.h — windows-sys puts them behind the
// Win32_UI_WindowsAndMessaging feature; not worth enabling for two
// integers (same treatment as the VK_* codes above).
const XBUTTON1: i32 = 1;
const XBUTTON2: i32 = 2;

/// Windows implementation: classify `action` into an [`Effect`] and
/// synthesise events via `SendInput`. macOS window-manager actions map to
/// their Windows equivalents; `CustomShortcut` maps macOS `kVK_*` codes to
/// Windows virtual-key codes (Cmd → Ctrl).
pub(super) fn execute(action: &Action) {
    match action.effect() {
        Effect::None => {}
        Effect::Click(button) => post_click(button),
        Effect::Shortcut(shortcut) => press_shortcut(shortcut),
        Effect::Key(combo) | Effect::HeldKey(combo) => post_custom_shortcut(combo),
        Effect::Scroll { dx, dy } => dispatch_scroll(dx, dy),
        Effect::Media(key) => dispatch_media(key),
        Effect::Native(native) => dispatch_native(native),
        Effect::Script(script) => dispatch_script(script),
        Effect::Text(text) => {
            tracing::warn!(
                chars = text.chars().count(),
                "TypeText injection is not implemented on Windows yet"
            );
        }
        Effect::AgentSide => {
            tracing::debug!(
                action = action.label(),
                "device action handled by hook/HID layer"
            );
        }
    }
}

/// The Windows chord for a named [`Shortcut`], or the raw virtual key to
/// post with no modifier when Windows has no chord-shaped representation
/// for it at all.
///
/// `BrowserBack`/`BrowserForward` are Windows' one exception: they fire a
/// dedicated virtual key (`VK_BROWSER_BACK`/`_FORWARD`) with no modifier,
/// which isn't a USB HID keyboard usage and so has no [`KeyCombo`]
/// representation — unlike on macOS/Linux, where the same shortcuts are
/// ordinary modifier+key chords. [`press_shortcut`] posts the `Err` case
/// directly instead of routing it through [`post_custom_shortcut`].
fn combo(shortcut: Shortcut) -> Result<KeyCombo, u16> {
    let text = match shortcut {
        Shortcut::BrowserBack => return Err(VK_BROWSER_BACK),
        Shortcut::BrowserForward => return Err(VK_BROWSER_FORWARD),
        Shortcut::Copy => "Ctrl+C",
        Shortcut::Paste => "Ctrl+V",
        Shortcut::Cut => "Ctrl+X",
        Shortcut::Undo => "Ctrl+Z",
        // Ctrl+Y, not Ctrl+Shift+Z: matches the dominant Windows convention
        // (Office, most Win32/UWP apps) rather than the macOS ⌘⇧Z one.
        Shortcut::Redo => "Ctrl+Y",
        Shortcut::SelectAll => "Ctrl+A",
        Shortcut::Find => "Ctrl+F",
        Shortcut::Save => "Ctrl+S",
        Shortcut::NewTab => "Ctrl+T",
        Shortcut::CloseTab => "Ctrl+W",
        Shortcut::ReopenTab => "Ctrl+Shift+T",
        Shortcut::NextTab => "Ctrl+Tab",
        Shortcut::PrevTab => "Ctrl+Shift+Tab",
        Shortcut::ReloadPage => "Ctrl+R",
    };
    Ok(parse_shortcut(text))
}

fn parse_shortcut(text: &str) -> KeyCombo {
    text.parse()
        .unwrap_or_else(|error| unreachable!("hardcoded shortcut table entry {text:?}: {error}"))
}

fn press_shortcut(shortcut: Shortcut) {
    match combo(shortcut) {
        Ok(combo) => post_custom_shortcut(&combo),
        Err(vk) => post_key(vk, &[]),
    }
}

/// Dispatch a window-manager or power [`NativeAction`]. macOS window-manager
/// concepts map to their nearest Windows shortcut; `Sleep` has no clean
/// synthesis (see the comment below) and is skipped.
fn dispatch_native(native: NativeAction) {
    match native {
        NativeAction::MissionControl | NativeAction::AppExpose => post_key(VK_TAB, &[VK_LWIN]),
        NativeAction::PreviousDesktop => post_key(VK_LEFT, &[VK_LWIN, VK_CONTROL]),
        NativeAction::NextDesktop => post_key(VK_RIGHT, &[VK_LWIN, VK_CONTROL]),
        NativeAction::ShowDesktop => post_key(VK_D, &[VK_LWIN]),
        NativeAction::LaunchpadShow => post_key(VK_LWIN, &[]),
        NativeAction::LockScreen => post_key(VK_L, &[VK_LWIN]),
        // Win+Shift+S opens the snip overlay, which serves both full-screen
        // and region capture on Windows.
        NativeAction::Screenshot | NativeAction::CaptureRegion => {
            post_key(VK_S, &[VK_LWIN, VK_SHIFT]);
        }
        // Suspending reliably needs `SetSuspendState` (powrprof.dll), which
        // hibernates instead when hibernation is enabled — no clean win from
        // a background agent, so the action is skipped on Windows for now.
        NativeAction::Sleep => {
            tracing::debug!("Sleep has no Windows synthesis yet — action skipped");
        }
    }
}

fn dispatch_media(key: MediaKey) {
    match key {
        MediaKey::PlayPause => post_key(VK_MEDIA_PLAY_PAUSE, &[]),
        MediaKey::NextTrack => post_key(VK_MEDIA_NEXT_TRACK, &[]),
        MediaKey::PrevTrack => post_key(VK_MEDIA_PREV_TRACK, &[]),
        MediaKey::VolumeUp => post_key(VK_VOLUME_UP, &[]),
        MediaKey::VolumeDown => post_key(VK_VOLUME_DOWN, &[]),
        MediaKey::Mute => post_key(VK_VOLUME_MUTE, &[]),
    }
}

fn dispatch_script(script: Script<'_>) {
    match script {
        Script::AppleScript(_) => {
            tracing::warn!("RunAppleScript is only supported on macOS");
        }
        Script::ShellCommand(cmd) => run_shell_command_async(cmd.to_string()),
        Script::Workflow(steps) => run_workflow_async(steps.to_vec()),
    }
}

fn run_shell_command_async(cmd: String) {
    std::thread::spawn(move || run_shell_command(&cmd));
}

fn run_workflow_async(steps: Vec<WorkflowStep>) {
    std::thread::spawn(move || run_workflow(&steps));
}

fn run_workflow(steps: &[WorkflowStep]) {
    for step in steps {
        match step {
            WorkflowStep::TypeText(text) => {
                tracing::warn!(
                    chars = text.chars().count(),
                    "workflow TypeText injection is not implemented on Windows yet"
                );
            }
            WorkflowStep::PressKey(combo) => post_custom_shortcut(combo),
            WorkflowStep::Delay { millis } => {
                std::thread::sleep(std::time::Duration::from_millis(*millis));
            }
            WorkflowStep::RunAppleScript(_) => {
                tracing::warn!("workflow RunAppleScript is only supported on macOS");
            }
            WorkflowStep::RunShellCommand(cmd) => run_shell_command(cmd),
        }
    }
}

fn run_shell_command(cmd: &str) {
    let _ = std::process::Command::new("cmd").args(["/C", cmd]).output();
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

/// Synthesise one scroll tick in direction `(dx, dy)`. Unit direction
/// (-1/0/1) scaled by `WHEEL_DELTA`, the fixed magnitude the four
/// `Scroll*`/`HorizontalScroll*` actions have always used.
fn dispatch_scroll(dx: i8, dy: i8) {
    let mut inputs = Vec::with_capacity(2);
    if dy != 0 {
        inputs.push(mouse_input(MOUSEEVENTF_WHEEL, i32::from(dy) * WHEEL_DELTA));
    }
    if dx != 0 {
        inputs.push(mouse_input(MOUSEEVENTF_HWHEEL, i32::from(dx) * WHEEL_DELTA));
    }
    if !inputs.is_empty() {
        send_inputs(&inputs);
    }
}

pub(super) fn post_scroll(delta: ScrollDelta) {
    let ScrollDelta::WheelTicks { .. } = delta else {
        tracing::debug!("pixel scroll output is unsupported on Windows");
        return;
    };
    let Ok(mut quantizer) = SCROLL_QUANTIZER.lock() else {
        tracing::warn!("Windows scroll quantizer mutex poisoned");
        return;
    };
    let delta = quantizer.quantize(delta, WHEEL_DELTA_F64);
    drop(quantizer);

    let mut inputs = Vec::with_capacity(2);
    if delta.y != 0 {
        inputs.push(mouse_input(MOUSEEVENTF_WHEEL, delta.y));
    }
    if delta.x != 0 {
        inputs.push(mouse_input(MOUSEEVENTF_HWHEEL, delta.x));
    }
    if !inputs.is_empty() {
        send_inputs(&inputs);
    }
}

fn post_custom_shortcut(combo: &KeyCombo) {
    let Some(vk) = super::hid_usage_to_windows(combo.key().code()) else {
        tracing::warn!(
            usage = combo.key().code(),
            chord = %combo.rendered_label(),
            "CustomShortcut key has no Windows mapping yet; press ignored"
        );
        return;
    };

    post_key(vk, &combo_modifiers(combo));
}

fn combo_modifiers(combo: &KeyCombo) -> Vec<u16> {
    let mut modifiers = Vec::new();
    if combo.has_command() {
        modifiers.push(VK_LWIN);
    }
    if combo.has_shift() {
        modifiers.push(VK_SHIFT);
    }
    if combo.has_control() && !modifiers.contains(&VK_CONTROL) {
        modifiers.push(VK_CONTROL);
    }
    if combo.has_option() {
        modifiers.push(VK_MENU);
    }
    modifiers
}

/// Emit one edge for the physical keys whose ownership changed.
pub(super) fn hold_keys(keys: &[HeldKey], phase: KeyPhase) {
    let keys: Vec<_> = keys
        .iter()
        .filter_map(|key| held_virtual_key(*key))
        .collect();
    let key_up = phase == KeyPhase::Up;
    let mut inputs: Vec<_> = keys.iter().map(|key| key_input(*key, key_up)).collect();
    if key_up {
        inputs.reverse();
    }
    send_inputs(&inputs);
}

fn held_virtual_key(key: HeldKey) -> Option<u16> {
    match key {
        HeldKey::Control => Some(VK_CONTROL),
        HeldKey::Shift => Some(VK_SHIFT),
        HeldKey::Alt => Some(VK_MENU),
        HeldKey::Key(usage) => {
            let key = super::hid_usage_to_windows(usage.code());
            if key.is_none() {
                tracing::warn!(
                    usage = usage.code(),
                    "held shortcut usage has no Windows mapping — edge ignored"
                );
            }
            key
        }
    }
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
    use openlogi_core::binding::Shortcut;

    use super::{VK_BROWSER_BACK, VK_BROWSER_FORWARD, combo};

    /// Pin a handful of representative `Shortcut -> KeyCombo` rows so an
    /// edit to the table can't silently change what Ctrl+C sends.
    /// `Redo` differs from macOS/Linux by design (see the module doc on
    /// `combo`), and `BrowserBack`/`BrowserForward` have no `KeyCombo`
    /// representation on Windows at all — see `combo`'s doc.
    #[test]
    fn combo_table_pins_representative_shortcuts() {
        assert_eq!(
            combo(Shortcut::Copy)
                .unwrap_or_else(|vk| panic!("Copy should be a chord, not raw vk {vk:#x}"))
                .rendered_label(),
            "Ctrl+C"
        );
        assert_eq!(
            combo(Shortcut::Redo)
                .unwrap_or_else(|vk| panic!("Redo should be a chord, not raw vk {vk:#x}"))
                .rendered_label(),
            "Ctrl+Y"
        );
        assert_eq!(
            combo(Shortcut::NextTab)
                .unwrap_or_else(|vk| panic!("NextTab should be a chord, not raw vk {vk:#x}"))
                .rendered_label(),
            "Ctrl+Tab"
        );
        assert_eq!(combo(Shortcut::BrowserBack), Err(VK_BROWSER_BACK));
        assert_eq!(combo(Shortcut::BrowserForward), Err(VK_BROWSER_FORWARD));
        // Every chord-shaped row must actually resolve through
        // hid_usage_to_windows, or a `Shortcut` silently no-ops instead of
        // pressing anything (see `post_custom_shortcut`'s warn-and-drop
        // path). Iterates `Shortcut::ALL` rather than a hand-copied list,
        // so a newly added `Shortcut` variant is checked here
        // automatically instead of depending on someone remembering to
        // extend a second, independent list. The two raw-vk exceptions
        // (pinned individually above) are skipped here, not silently
        // missed: `combo`'s own match already forces every `Shortcut`
        // variant to be classified as one or the other.
        for &shortcut in Shortcut::ALL {
            let Ok(chord) = combo(shortcut) else {
                continue;
            };
            let key = chord.key().code();
            assert!(
                super::super::hid_usage_to_windows(key).is_some(),
                "{shortcut:?} table entry has no Windows virtual-key mapping"
            );
        }
    }
}
