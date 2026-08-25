//! OS input-event synthesis for each [`Action`], split out of openlogi-core so
//! the core schema stays platform- and IO-free.
//!
//! [`execute`] is the single entry point: it dispatches to the per-platform
//! synthesiser (`macos::execute` / `linux::execute` / `windows::execute`), each
//! of which translates an [`Action`] into the native event(s) — CGEvent/NSEvent
//! on macOS, uinput/D-Bus on Linux, SendInput on Windows.

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use std::collections::HashMap;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use std::sync::{LazyLock, Mutex, PoisonError};

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use openlogi_core::binding::KeyboardUsage;
use openlogi_core::binding::{Action, KeyCombo};

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "windows")]
mod windows;

/// Which isolated edge of a held keyboard chord to synthesize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyPhase {
    Down,
    Up,
}

/// One physical keyboard output shared by held chords.
///
/// Logical Cmd and Ctrl are distinct on macOS. Cmd aliases Ctrl on Linux and
/// Windows, so ownership is counted after that platform mapping is resolved.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum HeldKey {
    #[cfg(target_os = "macos")]
    Command,
    Control,
    Shift,
    Alt,
    Key(KeyboardUsage),
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[derive(Debug, Default, PartialEq, Eq)]
struct HoldTransition {
    up: Vec<HeldKey>,
    down: Vec<HeldKey>,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HeldModifiers(u8);

#[cfg(target_os = "macos")]
impl HeldModifiers {
    fn set(&mut self, key: HeldKey, held: bool) {
        let Some(mask) = Self::mask(key) else {
            return;
        };
        if held {
            self.0 |= mask;
        } else {
            self.0 &= !mask;
        }
    }

    fn contains(self, key: HeldKey) -> bool {
        Self::mask(key).is_some_and(|mask| self.0 & mask != 0)
    }

    fn mask(key: HeldKey) -> Option<u8> {
        match key {
            HeldKey::Command => Some(1 << 0),
            HeldKey::Control => Some(1 << 1),
            HeldKey::Shift => Some(1 << 2),
            HeldKey::Alt => Some(1 << 3),
            HeldKey::Key(_) => None,
        }
    }
}

/// Reference counts for physical keyboard outputs across active chords.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[derive(Default)]
struct HeldOutput {
    owners: HashMap<HeldKey, usize>,
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
impl HeldOutput {
    fn transition(
        &mut self,
        released: Option<&KeyCombo>,
        pressed: Option<&KeyCombo>,
    ) -> HoldTransition {
        let released = released.map_or_else(Vec::new, held_keys);
        let pressed = pressed.map_or_else(Vec::new, held_keys);
        let before = self.owners.clone();

        for key in &released {
            match self.owners.get_mut(key) {
                Some(owners) if *owners > 1 => *owners -= 1,
                Some(_) => {
                    self.owners.remove(key);
                }
                None => {}
            }
        }
        for key in &pressed {
            *self.owners.entry(*key).or_default() += 1;
        }

        HoldTransition {
            up: released
                .into_iter()
                .filter(|key| before.contains_key(key) && !self.owners.contains_key(key))
                .collect(),
            down: pressed
                .into_iter()
                .filter(|key| !before.contains_key(key) && self.owners.contains_key(key))
                .collect(),
        }
    }

    #[cfg(target_os = "macos")]
    fn modifiers(&self) -> HeldModifiers {
        let mut modifiers = HeldModifiers::default();
        for key in [
            HeldKey::Command,
            HeldKey::Control,
            HeldKey::Shift,
            HeldKey::Alt,
        ] {
            modifiers.set(key, self.owners.contains_key(&key));
        }
        modifiers
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
static HELD_OUTPUT: LazyLock<Mutex<HeldOutput>> =
    LazyLock::new(|| Mutex::new(HeldOutput::default()));

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn held_keys(combo: &KeyCombo) -> Vec<HeldKey> {
    let mut keys = Vec::with_capacity(4);
    #[cfg(target_os = "macos")]
    if combo.has_command() {
        keys.push(HeldKey::Command);
    }
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    if combo.has_command() || combo.has_control() {
        keys.push(HeldKey::Control);
    }
    #[cfg(target_os = "macos")]
    if combo.has_control() {
        keys.push(HeldKey::Control);
    }
    if combo.has_shift() {
        keys.push(HeldKey::Shift);
    }
    if combo.has_option() {
        keys.push(HeldKey::Alt);
    }
    keys.push(HeldKey::Key(combo.key()));
    keys
}

/// Synthesise the OS-level event for `action`.
///
/// On macOS, key events are posted via `CGEventPost(kCGHIDEventTap, …)`
/// using virtual key codes from the standard US keyboard layout, and the
/// `LeftClick`/`RightClick`/`MiddleClick` variants synthesise a mouse click
/// at the current cursor location. The WindowServer actions (`MissionControl`,
/// `AppExpose`, `ShowDesktop`, `LaunchpadShow`) are posted straight to the
/// Dock via `CoreDockSendNotification`. Device-side actions (`CycleDpiPresets`,
/// `SetDpiPreset`, `ToggleSmartShift`) have no CGEvent equivalent and are
/// handled at the hook/HID layer, logging a trace here.
///
/// On Linux, key and scroll events are injected via a lazily-created `uinput`
/// virtual device. Mouse clicks inject `BTN_*` events. macOS-only window
/// manager actions (`MissionControl`, `AppExpose`, `ShowDesktop`,
/// `LaunchpadShow`) have no universal Linux equivalent and are silently
/// skipped (debug-logged). `CustomShortcut` maps macOS `kVK_*` codes to
/// Linux key codes; macOS Cmd maps to Ctrl.
///
/// On Windows, key and mouse events are synthesised via `SendInput`. The
/// macOS window-manager actions map to their Windows equivalents (e.g.
/// `MissionControl` → Win+Tab, `ShowDesktop` → Win+D); `CustomShortcut`
/// maps macOS `kVK_*` codes to Windows virtual-key codes, with Cmd mapped to
/// Ctrl.
///
/// On other platforms a warning is logged and the function returns
/// immediately — the binary compiles clean on all targets.
///
/// # Manual verification
///
/// `execute` is intentionally excluded from the automated test suite because
/// it would need to intercept the OS event queue. Smoke-test it manually:
/// bind a button to any action in the GUI and confirm the expected system event
/// fires when the button is pressed (or use the `inject_action` example).
pub fn execute(action: &Action) {
    if let Action::OpenApplication(target) = action {
        let expanded = shellexpand::tilde(target.path());
        if let Err(error) = opener::open(expanded.as_ref()) {
            tracing::warn!(
                %error,
                path = target.path(),
                "could not open configured application, folder, or URL"
            );
        }
        return;
    }

    cfg_select! {
        target_os = "macos" => {
            macos::execute(action);
        }
        target_os = "linux" => {
            linux::execute(action);
        }
        target_os = "windows" => {
            windows::execute(action);
        }
        _ => {
            tracing::warn!(
                action = action.label(),
                "execute unsupported on this platform"
            );
        }
    }
}

/// Synthesise the down edge of `combo`, leaving its output held.
///
/// Every successful lifecycle start must be paired with [`release_hold`],
/// including cancellation and shutdown paths. Prefer [`execute`] when the
/// caller does not own a matching terminal event.
pub fn press_hold(combo: &KeyCombo) {
    hold_transition(None, Some(combo));
}

/// Synthesise the up edge matching a prior [`press_hold`].
///
/// Each successful [`press_hold`] must be released exactly once. The lifecycle
/// owner provides that guarantee; duplicate releases would consume ownership
/// retained for an overlapping chord.
pub fn release_hold(combo: &KeyCombo) {
    hold_transition(Some(combo), None);
}

/// Replace one held chord without releasing physical keys shared by both.
pub fn replace_hold(old: &KeyCombo, new: &KeyCombo) {
    hold_transition(Some(old), Some(new));
}

fn hold_transition(released: Option<&KeyCombo>, pressed: Option<&KeyCombo>) {
    cfg_select! {
        target_os = "macos" => {
            let mut output = HELD_OUTPUT.lock().unwrap_or_else(PoisonError::into_inner);
            let modifiers = output.modifiers();
            let transition = output.transition(released, pressed);
            let modifiers = macos::hold_keys(&transition.up, KeyPhase::Up, modifiers);
            let modifiers = macos::hold_keys(&transition.down, KeyPhase::Down, modifiers);
            debug_assert_eq!(modifiers, output.modifiers());
        }
        target_os = "linux" => {
            let mut output = HELD_OUTPUT.lock().unwrap_or_else(PoisonError::into_inner);
            let transition = output.transition(released, pressed);
            linux::hold_keys(&transition.up, KeyPhase::Up);
            linux::hold_keys(&transition.down, KeyPhase::Down);
        }
        target_os = "windows" => {
            let mut output = HELD_OUTPUT.lock().unwrap_or_else(PoisonError::into_inner);
            let transition = output.transition(released, pressed);
            windows::hold_keys(&transition.up, KeyPhase::Up);
            windows::hold_keys(&transition.down, KeyPhase::Down);
        }
        _ => {
            tracing::warn!(
                "held shortcut output unsupported on this platform"
            );
        }
    }
}

/// Navigate the browser identified by `pid` backwards or forwards using the
/// Accessibility API (`AXPress` on the "Go back" / "Go forward" toolbar button).
///
/// Call this from the gesture watcher **at the moment the button press arrives**
/// so `pid` reflects the correct frontmost app rather than whatever happens to
/// be frontmost when the async dispatch completes. Returns `true` on success.
/// No-op (returns `false`) on non-macOS platforms.
#[must_use]
pub fn ax_navigate_browser(pid: i32, forward: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::ax_browser_navigate(forward, Some(pid))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pid, forward);
        false
    }
}

/// Synthesise a horizontal scroll of `delta` wheel lines at the current focus.
///
/// Used by the gesture/thumbwheel capture watcher to re-inject the MX thumb
/// wheel's scrolling after the wheel has been diverted over HID++ to capture its
/// click. `delta` is the device's raw rotation; its sign follows the wheel's
/// rotation convention and its magnitude (one line per rotation increment) may
/// need tuning per device, since the diverted resolution differs from native.
///
/// No-op (logs nothing) on platforms without a supported injection mechanism.
pub fn post_horizontal_scroll(delta: i32) {
    cfg_select! {
        target_os = "macos" => {
            macos::post_horizontal_scroll(delta);
        }
        target_os = "linux" => {
            // `delta` is already in "one line per rotation increment" units (see
            // doc above), which matches REL_HWHEEL's convention of one unit per
            // detent. This is intentionally different from
            // Action::HorizontalScrollLeft/Right, which hardcode ±3 as a fixed
            // "scroll tick" with no device delta involved.
            linux::scroll(evdev::RelativeAxisCode::REL_HWHEEL, delta);
        }
        target_os = "windows" => {
            windows::post_horizontal_scroll(delta);
        }
        _ => {
            let _ = delta;
        }
    }
}

/// Return the `/dev/input/eventN` node for the action-injector uinput device,
/// initialising it if needed.
///
/// Intended for debugging and manual smoke-testing (e.g. attaching `evtest`
/// before firing [`execute`]). Returns `None` on non-Linux platforms or
/// when the device could not be created (e.g. `/dev/uinput` not writable).
#[cfg(target_os = "linux")]
#[must_use]
pub fn action_device_path() -> Option<std::path::PathBuf> {
    linux::device_node()
}

/// Stamped into the `EVENT_SOURCE_USER_DATA` field of every mouse event
/// [`execute`] synthesizes on macOS, so OpenLogi's own `CGEventTap` can
/// recognize and skip its own injections. Without it, a gesture/button action
/// that posts a mouse button (e.g. a remapped `MiddleClick`) would re-enter the
/// hook — and for a gesture button, be misread as a fresh hold, looping. The
/// value is arbitrary but distinctive ("OLGI"); real events carry `0` here.
pub const SYNTHETIC_EVENT_USER_DATA: i64 = 0x4F4C_4749;

/// Translate a platform-neutral USB HID keyboard usage to a Win32 virtual key.
// Not `expect`: the lint fires in the `--lib` build and not in the `--test`
// one, so an expectation is always unfulfilled for one of them.
#[cfg_attr(
    not(target_os = "windows"),
    expect(clippy::allow_attributes, reason = "see above"),
    allow(dead_code, reason = "called only by the Windows backend")
)]
fn hid_usage_to_windows(usage: u8) -> Option<u16> {
    match usage {
        0x04..=0x1d => Some(u16::from(b'A' + usage - 0x04)),
        0x1e..=0x26 => Some(u16::from(b'1' + usage - 0x1e)),
        0x27 => Some(u16::from(b'0')),
        0x3a..=0x45 => Some(0x70 + u16::from(usage - 0x3a)),
        0x68..=0x6f => Some(0x7c + u16::from(usage - 0x68)),
        0x28 => Some(0x0d),
        0x29 => Some(0x1b),
        0x2a => Some(0x08),
        0x2b => Some(0x09),
        0x2c => Some(0x20),
        0x2d => Some(0xbd),
        0x2e => Some(0xbb),
        0x2f => Some(0xdb),
        0x30 => Some(0xdd),
        0x31 => Some(0xdc),
        0x33 => Some(0xba),
        0x34 => Some(0xde),
        0x35 => Some(0xc0),
        0x36 => Some(0xbc),
        0x37 => Some(0xbe),
        0x38 => Some(0xbf),
        0x4a => Some(0x24),
        0x4b => Some(0x21),
        0x4c => Some(0x2e),
        0x4d => Some(0x23),
        0x4e => Some(0x22),
        0x4f => Some(0x27),
        0x50 => Some(0x25),
        0x51 => Some(0x28),
        0x52 => Some(0x26),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    use openlogi_core::binding::KeyCombo;

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    use super::{HeldKey, HeldOutput, HoldTransition};

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    fn combo(label: &str) -> KeyCombo {
        label.parse().expect("test shortcut must be valid")
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn shared_control_stays_down_until_its_last_chord_ends() {
        let control_a = combo("Ctrl+A");
        let control_b = combo("Ctrl+B");
        let mut output = HeldOutput::default();

        assert_eq!(
            output.transition(None, Some(&control_a)),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Control, HeldKey::Key(control_a.key())],
            }
        );
        assert_eq!(
            output.transition(None, Some(&control_b)),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Key(control_b.key())],
            }
        );
        assert_eq!(
            output.transition(Some(&control_a), None),
            HoldTransition {
                up: vec![HeldKey::Key(control_a.key())],
                down: vec![],
            }
        );
        assert_eq!(
            output.transition(Some(&control_b), None),
            HoldTransition {
                up: vec![HeldKey::Control, HeldKey::Key(control_b.key())],
                down: vec![],
            }
        );
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn command_and_control_share_one_physical_output() {
        let command_a = combo("Cmd+A");
        let control_b = combo("Ctrl+B");
        let mut output = HeldOutput::default();

        output.transition(None, Some(&command_a));
        assert_eq!(
            output.transition(None, Some(&control_b)),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Key(control_b.key())],
            }
        );
        assert_eq!(
            output.transition(Some(&command_a), None),
            HoldTransition {
                up: vec![HeldKey::Key(command_a.key())],
                down: vec![],
            }
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn command_and_control_are_distinct_physical_outputs() {
        let command_a = combo("Cmd+A");
        let control_b = combo("Ctrl+B");
        let mut output = HeldOutput::default();

        output.transition(None, Some(&command_a));
        assert_eq!(
            output.transition(None, Some(&control_b)),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Control, HeldKey::Key(control_b.key())],
            }
        );
        assert_eq!(
            output.transition(Some(&command_a), None),
            HoldTransition {
                up: vec![HeldKey::Command, HeldKey::Key(command_a.key())],
                down: vec![],
            }
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn shared_command_stays_down_until_its_last_chord_ends() {
        let command_a = combo("Cmd+A");
        let command_b = combo("Cmd+B");
        let mut output = HeldOutput::default();

        output.transition(None, Some(&command_a));
        assert_eq!(
            output.transition(None, Some(&command_b)),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Key(command_b.key())],
            }
        );
        assert_eq!(
            output.transition(Some(&command_a), None),
            HoldTransition {
                up: vec![HeldKey::Key(command_a.key())],
                down: vec![],
            }
        );
        assert_eq!(
            output.transition(Some(&command_b), None),
            HoldTransition {
                up: vec![HeldKey::Command, HeldKey::Key(command_b.key())],
                down: vec![],
            }
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn replacement_preserves_shared_physical_outputs() {
        let old = combo("Ctrl+A");
        let new = combo("Ctrl+B");
        let mut output = HeldOutput::default();

        output.transition(None, Some(&old));
        assert_eq!(
            output.transition(Some(&old), Some(&new)),
            HoldTransition {
                up: vec![HeldKey::Key(old.key())],
                down: vec![HeldKey::Key(new.key())],
            }
        );
    }

    #[test]
    fn hid_usages_map_across_windows_key_categories() {
        use super::hid_usage_to_windows;

        assert_eq!(hid_usage_to_windows(0x04), Some(0x41)); // A
        assert_eq!(hid_usage_to_windows(0x1e), Some(0x31)); // 1
        assert_eq!(hid_usage_to_windows(0x3a), Some(0x70)); // F1
        assert_eq!(hid_usage_to_windows(0x6f), Some(0x83)); // F20
        assert_eq!(hid_usage_to_windows(0x50), Some(0x25)); // Left
        assert_eq!(hid_usage_to_windows(0x2c), Some(0x20)); // Space
        assert_eq!(hid_usage_to_windows(0x33), Some(0xba)); // Semicolon
        assert_eq!(hid_usage_to_windows(0xff), None);
    }
}
