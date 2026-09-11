//! OS input-event synthesis for each [`Action`], split out of openlogi-core so
//! the core schema stays platform- and IO-free.
//!
//! [`execute`] is the single entry point: it dispatches to the per-platform
//! synthesiser (`macos::execute` / `linux::execute` / `windows::execute`), each
//! of which translates an [`Action`] into the native event(s) — CGEvent/NSEvent
//! on macOS, uinput/D-Bus on Linux, SendInput on Windows.

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use std::collections::HashMap;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use std::sync::{LazyLock, Mutex, PoisonError};

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use openlogi_core::binding::KeyboardUsage;
use openlogi_core::binding::{Action, HeldKeys, KeyCombo, Modifiers};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use openlogi_core::binding::{Script, WorkflowStep};
use openlogi_core::scroll::ScrollDelta;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(any(target_os = "macos", test))]
mod space_switch;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
use windows as platform;

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
    /// The platform logo key: Linux `KEY_LEFTMETA` or the Windows key. On macOS
    /// the logo key *is* Command, so `Super` chords own `HeldKey::Command`
    /// there — one physical key, one owner — exactly as `Cmd` chords own
    /// `HeldKey::Control` on Linux and Windows.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    Super,
    Control,
    Shift,
    Alt,
    Key(KeyboardUsage),
}

/// Owned shape of one hold: a chord or a bare-modifier set. Not public;
/// callers reach it through [`press_hold`] or [`press_hold_modifier`], which
/// wrap it in a [`HeldChord`].
///
/// Ungated even though [`HeldKey`] is not: `hold_transition` and
/// [`HeldChord`] name this type on every target, and a platform without a
/// synthesis backend still has to compile them.
enum HeldChordKind {
    Chord(KeyCombo, HeldKeys),
    Modifiers(Modifiers),
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
impl HeldChordKind {
    fn held_keys(&self) -> Vec<HeldKey> {
        match self {
            Self::Chord(combo, keys) => held_keys(combo, *keys),
            Self::Modifiers(mods) => held_keys_for_modifiers(*mods),
        }
    }

    /// The ordinary key a starting [`HeldKeys::ModifiersOnly`] chord taps once.
    fn tapped_key(&self) -> Option<HeldKey> {
        match self {
            Self::Chord(combo, HeldKeys::ModifiersOnly) => Some(HeldKey::Key(combo.key())),
            Self::Chord(_, HeldKeys::WholeChord) | Self::Modifiers(_) => None,
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[derive(Debug, Default, PartialEq, Eq)]
struct HoldTransition {
    up: Vec<HeldKey>,
    down: Vec<HeldKey>,
    /// The ordinary key a starting [`HeldKeys::ModifiersOnly`] hold taps once,
    /// posted after `down`. `None` when the hold taps nothing, including when
    /// another hold already owns that key: its up edge would release the other
    /// hold's key early.
    tap: Option<HeldKey>,
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
    /// Owners counted from bare-modifier holds only, so the tap mirror a
    /// [`HeldChordKind::Modifiers`] hold publishes never carries a held
    /// chord's modifiers — a chord hold does not modify other input.
    #[cfg(target_os = "macos")]
    modifier_hold_owners: HashMap<HeldKey, usize>,
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
impl HeldOutput {
    fn transition(
        &mut self,
        released: Option<&HeldChordKind>,
        pressed: Option<&HeldChordKind>,
    ) -> HoldTransition {
        #[cfg(target_os = "macos")]
        self.transition_modifier_holds(released, pressed);
        let tapped = pressed.and_then(HeldChordKind::tapped_key);
        let released = released.map_or_else(Vec::new, HeldChordKind::held_keys);
        let pressed = pressed.map_or_else(Vec::new, HeldChordKind::held_keys);
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
            // A modifiers-only hold never owns its ordinary key, so an owner
            // here is another hold keeping that key down. Leave it alone.
            tap: tapped.filter(|key| !self.owners.contains_key(key)),
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

    /// Advance the bare-modifier-hold reference counts through one transition.
    #[cfg(target_os = "macos")]
    fn transition_modifier_holds(
        &mut self,
        released: Option<&HeldChordKind>,
        pressed: Option<&HeldChordKind>,
    ) {
        if let Some(input @ HeldChordKind::Modifiers(_)) = released {
            for key in input.held_keys() {
                match self.modifier_hold_owners.get_mut(&key) {
                    Some(owners) if *owners > 1 => *owners -= 1,
                    Some(_) => {
                        self.modifier_hold_owners.remove(&key);
                    }
                    None => {}
                }
            }
        }
        if let Some(input @ HeldChordKind::Modifiers(_)) = pressed {
            for key in input.held_keys() {
                *self.modifier_hold_owners.entry(key).or_default() += 1;
            }
        }
    }

    /// The modifiers currently owned by at least one bare-modifier hold.
    #[cfg(target_os = "macos")]
    fn modifier_hold_modifiers(&self) -> HeldModifiers {
        let mut modifiers = HeldModifiers::default();
        for key in [
            HeldKey::Command,
            HeldKey::Control,
            HeldKey::Shift,
            HeldKey::Alt,
        ] {
            modifiers.set(key, self.modifier_hold_owners.contains_key(&key));
        }
        modifiers
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
static HELD_OUTPUT: LazyLock<Mutex<HeldOutput>> =
    LazyLock::new(|| Mutex::new(HeldOutput::default()));

/// `CGEventFlags` mirror of the bare-modifier holds in [`HELD_OUTPUT`],
/// written only by [`hold_transition`] under that same lock. Derived state,
/// not a second authority: it exists because the event tap callback that
/// consumes it must never block on a mutex.
#[cfg(target_os = "macos")]
static HELD_MODIFIER_FLAGS: AtomicU64 = AtomicU64::new(0);

/// `CGEventFlags` bits for the modifiers currently held by bare-modifier
/// holds ([`press_hold_modifier`]), for the macOS event tap to OR into
/// passing events.
///
/// Posting a modifier key-down reaches only consumers that *poll* modifier
/// state; consumers that read flags off the events they receive — keystrokes,
/// scroll, clicks, i.e. almost everything — see a synthetic modifier only
/// when the tap stamps it onto those events. Held chords never contribute
/// here: their contract is "hold this chord", not "modify my other input".
#[cfg(target_os = "macos")]
#[must_use]
pub fn held_modifier_flags() -> u64 {
    HELD_MODIFIER_FLAGS.load(Ordering::Relaxed)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn held_keys(combo: &KeyCombo, held: HeldKeys) -> Vec<HeldKey> {
    let mut keys = combo
        .modifiers()
        .map_or_else(Vec::new, held_keys_for_modifiers);
    if held == HeldKeys::WholeChord {
        keys.push(HeldKey::Key(combo.key()));
    }
    keys
}

/// The physical modifier outputs for `mods`, after the per-platform mapping:
/// Cmd is Control on Linux and Windows, and Super is Command on macOS. Shared
/// by chords and bare-modifier holds, so both resolve a modifier to the same
/// owner. Emits no [`HeldKey::Key`], so a modifier-only hold cannot own an
/// ordinary key and therefore cannot release one a concurrent chord still
/// holds.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn held_keys_for_modifiers(mods: Modifiers) -> Vec<HeldKey> {
    let mut keys = Vec::with_capacity(4);
    #[cfg(target_os = "macos")]
    if mods.has_command() || mods.has_super() {
        keys.push(HeldKey::Command);
    }
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    if mods.has_command() || mods.has_control() {
        keys.push(HeldKey::Control);
    }
    #[cfg(target_os = "macos")]
    if mods.has_control() {
        keys.push(HeldKey::Control);
    }
    if mods.has_shift() {
        keys.push(HeldKey::Shift);
    }
    if mods.has_option() {
        keys.push(HeldKey::Alt);
    }
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    if mods.has_super() {
        keys.push(HeldKey::Super);
    }
    keys
}

/// A shortcut-table entry, parsed once into the chord it names. The tables are
/// hand-written constants, so a parse failure is a programming error.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn parse_shortcut(text: &str) -> KeyCombo {
    text.parse()
        .unwrap_or_else(|error| unreachable!("hardcoded shortcut table entry {text:?}: {error}"))
}

/// Run a script off the caller's thread: a shell command, an AppleScript or a
/// workflow can take seconds, and the caller is the input hook.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn dispatch_script(script: Script<'_>) {
    match script {
        Script::AppleScript(src) => {
            let src = src.to_string();
            std::thread::spawn(move || platform::run_apple_script(&src));
        }
        Script::ShellCommand(cmd) => {
            let cmd = cmd.to_string();
            std::thread::spawn(move || platform::run_shell_command(&cmd));
        }
        Script::Workflow(steps) => {
            let steps = steps.to_vec();
            std::thread::spawn(move || run_workflow(&steps));
        }
    }
}

/// Run workflow steps in order on the current (worker) thread, so a `Delay`
/// never stalls the event tap. Each step is one call into the platform
/// backend; a backend that cannot perform a step logs and moves on.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn run_workflow(steps: &[WorkflowStep]) {
    for step in steps {
        match step {
            WorkflowStep::TypeText(text) => platform::type_text(text),
            WorkflowStep::PressKey(combo) => platform::press_combo(combo),
            WorkflowStep::Delay { millis } => {
                std::thread::sleep(std::time::Duration::from_millis(*millis));
            }
            WorkflowStep::RunAppleScript(src) => platform::run_apple_script(src),
            WorkflowStep::RunShellCommand(cmd) => platform::run_shell_command(cmd),
        }
    }
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
/// macOS `PreviousDesktop` / `NextDesktop` send a DockSwipe without modifying
/// system shortcuts. This call waits for posting (or cancellation), preserving
/// event order for sequential callers, but not for the desktop animation to end.
/// A dedicated worker confirms the pointer display's Space; the process must
/// remain alive to receive that diagnostic. Overlapping switches are skipped,
/// not queued. Call from an action worker, never an input-tap callback.
/// Worker preparation has a two-second cancellation deadline; a canceled
/// worker cannot post later and retains its busy slot until it exits. Initial
/// cursor capture and committed native post calls are not interruptible: this
/// is not a hard wall-clock limit on `execute`.
///
/// On Linux, key and scroll events are injected via a lazily-created `uinput`
/// virtual device. Mouse clicks inject `BTN_*` events. When
/// `HYPRLAND_INSTANCE_SIGNATURE` is non-empty, `LaunchpadShow` runs
/// `omarchy-menu toggle`, desktop switches run `hyprctl dispatch workspace`,
/// and `LockScreen` runs `omarchy-system-lock` when available (never Super+L;
/// the fallback is Super+Ctrl+L). Helpers run asynchronously with a bounded
/// wait, then fall back to their compositor shortcut. Otherwise macOS-only
/// window manager actions (`MissionControl`, `AppExpose`, `ShowDesktop`,
/// `LaunchpadShow`) have no universal Linux equivalent and are silently skipped
/// (debug-logged). `CustomShortcut` maps macOS `kVK_*` codes to Linux key codes;
/// macOS Cmd maps to Ctrl.
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
/// For macOS Space switching, use the opt-in native test in a logged-in session
/// with Accessibility granted to the test host and a right-hand adjacent Space:
/// `cargo test -p openlogi-inject interactive_space_round_trip -- --ignored --nocapture`.
/// This actually switches the pointer's display right, then left back to the
/// original Space; a failure can leave it on the next Space.
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

/// One synthetic held chord, released exactly once when dropped.
///
/// Keep this value with the physical press lifecycle. Replacing its chord
/// preserves physical keys shared by the old and new chords; cancellation,
/// shutdown, and unwinding all release the current chord through [`Drop`].
///
/// A bare-modifier set ([`press_hold_modifier`]) is held the same way and
/// shares the same reference-counted ownership, so a modifier held by both a
/// chord and a modifier set stays down until the last holder releases it.
#[must_use = "dropping the held chord immediately releases its synthetic output"]
pub struct HeldChord {
    kind: HeldChordKind,
}

impl HeldChord {
    /// Replace this held chord without releasing physical keys shared by both.
    ///
    /// A replacement is a fresh hold of `combo`, so a
    /// [`HeldKeys::ModifiersOnly`] replacement taps its ordinary key the same
    /// way the original press did.
    pub fn replace(&mut self, combo: &KeyCombo, keys: HeldKeys) {
        let old = std::mem::replace(&mut self.kind, HeldChordKind::Chord(combo.clone(), keys));
        hold_transition(Some(&old), Some(&self.kind));
    }

    /// Replace this hold with a bare-modifier set, preserving outputs shared
    /// by both.
    pub fn replace_modifier(&mut self, mods: Modifiers) {
        let old = std::mem::replace(&mut self.kind, HeldChordKind::Modifiers(mods));
        hold_transition(Some(&old), Some(&self.kind));
    }
}

impl Drop for HeldChord {
    fn drop(&mut self) {
        hold_transition(Some(&self.kind), None);
    }
}

/// Synthesise the down edge of `combo` and return its release owner.
///
/// `keys` selects which of the chord's keys stay down: [`HeldKeys::WholeChord`]
/// holds the ordinary key with the modifiers, while
/// [`HeldKeys::ModifiersOnly`] taps it once here and holds only the modifiers.
/// The tap is skipped while another held chord keeps that ordinary key down,
/// because its up edge would release the other chord's key early.
///
/// Keep the returned [`HeldChord`] until the physical press ends. Prefer
/// [`execute`] when the caller does not own a matching terminal event.
pub fn press_hold(combo: &KeyCombo, keys: HeldKeys) -> HeldChord {
    // Construct the owner before posting the edge so unwinding from the
    // platform backend still balances any ownership transition it completed.
    let held = HeldChord {
        kind: HeldChordKind::Chord(combo.clone(), keys),
    };
    hold_transition(None, Some(&held.kind));
    held
}

/// Synthesise the down edge of a bare-modifier hold and return its release
/// owner.
///
/// The modifiers stay down for the lifetime of the returned [`HeldChord`] and
/// compose with input from other devices — holding Cmd here means a keystroke
/// on the physical keyboard arrives as Cmd+key.
pub fn press_hold_modifier(mods: Modifiers) -> HeldChord {
    let held = HeldChord {
        kind: HeldChordKind::Modifiers(mods),
    };
    hold_transition(None, Some(&held.kind));
    held
}

fn hold_transition(released: Option<&HeldChordKind>, pressed: Option<&HeldChordKind>) {
    cfg_select! {
        target_os = "macos" => {
            let mut output = HELD_OUTPUT.lock().unwrap_or_else(PoisonError::into_inner);
            // `HeldOutput::owners` is the only persistent modifier state. This
            // bitmask is an event-ordering cursor: derive it from the map while
            // holding the same mutex, advance it through the exact transition
            // edges, then prove it reached the map's post-transition state.
            let modifiers = output.modifiers();
            let transition = output.transition(released, pressed);
            // Publish the tap mirror before posting any edge: a released
            // modifier must leave the mirror before its own key-up travels
            // through the tap, or the tap would stamp the modifier straight
            // back onto that event.
            HELD_MODIFIER_FLAGS.store(
                macos::held_modifier_flags(output.modifier_hold_modifiers()).bits(),
                Ordering::Relaxed,
            );
            let modifiers = macos::hold_keys(&transition.up, KeyPhase::Up, modifiers);
            let modifiers = macos::hold_keys(&transition.down, KeyPhase::Down, modifiers);
            debug_assert_eq!(modifiers, output.modifiers());
            // The tap rides on the modifiers just posted, and owns nothing:
            // its own down/up pair cancel out, leaving the cursor where the
            // held state already is.
            if let Some(key) = transition.tap {
                let tapped = macos::hold_keys(&[key], KeyPhase::Down, modifiers);
                let tapped = macos::hold_keys(&[key], KeyPhase::Up, tapped);
                debug_assert_eq!(tapped, output.modifiers());
            }
        }
        target_os = "linux" => {
            let mut output = HELD_OUTPUT.lock().unwrap_or_else(PoisonError::into_inner);
            let transition = output.transition(released, pressed);
            linux::hold_keys(&transition.up, KeyPhase::Up);
            linux::hold_keys(&transition.down, KeyPhase::Down);
            if let Some(key) = transition.tap {
                linux::hold_keys(&[key], KeyPhase::Down);
                linux::hold_keys(&[key], KeyPhase::Up);
            }
        }
        target_os = "windows" => {
            let mut output = HELD_OUTPUT.lock().unwrap_or_else(PoisonError::into_inner);
            let transition = output.transition(released, pressed);
            windows::hold_keys(&transition.up, KeyPhase::Up);
            windows::hold_keys(&transition.down, KeyPhase::Down);
            if let Some(key) = transition.tap {
                windows::hold_keys(&[key], KeyPhase::Down);
                windows::hold_keys(&[key], KeyPhase::Up);
            }
        }
        _ => {
            tracing::warn!(
                "held shortcut output unsupported on this platform"
            );
        }
    }
}

/// Let the frontmost app's menus answer shortcuts the keyboard layout
/// remaps, such as Back and Forward on Spanish or German layouts. Call once
/// on the main thread before its run loop starts.
#[cfg(target_os = "macos")]
pub fn prepare_menu_shortcuts(mtm: objc2::MainThreadMarker) {
    macos::prepare_menu_shortcuts(mtm);
}

/// Navigate Safari backwards or forwards using `AXPress` on its toolbar
/// button's stable Accessibility identifier.
///
/// Pass the Safari process captured when the button press arrived. The call
/// returns `false` if that process is no longer frontmost or the frontmost app
/// is not Safari.
/// No-op (returns `false`) on non-macOS platforms.
#[must_use]
pub fn ax_navigate_browser(pid: i32, forward: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::ax_browser_navigate(forward, pid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pid, forward);
        false
    }
}

/// Integer scroll units ready for a platform API.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct QuantizedScroll {
    x: i32,
    y: i32,
}

/// Carries fractional platform units across frames so rounding never changes
/// the cumulative distance.
#[derive(Default)]
struct ScrollQuantizer {
    residual_x: f64,
    residual_y: f64,
}

impl ScrollQuantizer {
    fn quantize(&mut self, delta: ScrollDelta, units_per_input: f64) -> QuantizedScroll {
        QuantizedScroll {
            x: quantize_axis(&mut self.residual_x, delta.x(), units_per_input),
            y: quantize_axis(&mut self.residual_y, delta.y(), units_per_input),
        }
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the rounded value is clamped to the i32 range before conversion"
)]
fn quantize_axis(residual: &mut f64, input: f64, units_per_input: f64) -> i32 {
    let exact = input.mul_add(units_per_input, *residual);
    let rounded = exact
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX));
    let output = rounded as i32;
    *residual = exact - f64::from(output);
    output
}

/// Whether the OS reverses the scrolling a mouse wheel produces — macOS's
/// "Natural scrolling", on by default — read fresh on every call because the
/// user can change it while the agent runs.
///
/// The OS applies it to wheel events from a device, but not to the events
/// [`post_scroll`] and [`post_smooth_scroll`] synthesise. A caller posting in
/// place of a diverted wheel flips its own distance with this. `false` on
/// Windows and Linux, which apply no such reversal to a mouse wheel's
/// events here.
#[must_use]
pub fn natural_scrolling() -> bool {
    cfg_select! {
        target_os = "macos" => {
            objc2::rc::autoreleasepool(|_| macos::natural_scrolling())
        }
        _ => {
            false
        }
    }
}

/// Synthesise a typed scroll distance at the current focus.
///
/// Fractional wheel ticks are retained until the platform can represent them,
/// so a sequence of high-resolution frames preserves its cumulative distance.
/// Non-finite input is rejected at this I/O boundary.
pub fn post_scroll(delta: ScrollDelta) {
    if !delta.is_finite() || (delta.x() == 0.0 && delta.y() == 0.0) {
        return;
    }
    cfg_select! {
        target_os = "macos" => {
            macos::post_scroll(delta);
        }
        target_os = "linux" => {
            linux::post_scroll(delta);
        }
        target_os = "windows" => {
            windows::post_scroll(delta);
        }
        _ => {
            let _ = delta;
        }
    }
}

/// Lifecycle phase of one synthetic smooth-scroll frame.
///
/// Part of the runtime contract — the motion engine emits a balanced
/// lifecycle. Smoothed wheel output ([`post_smooth_scroll`]) does not forward
/// it: macOS deliberately posts *phaseless* continuous events there (matching
/// wheel semantics), because stamping `kCGScrollWheelEventScrollPhase`
/// declares a trackpad gesture, which enables rubber-band overscroll and
/// WebKit gesture latching that breaks JS-scrolled sites. Only an intended
/// gesture ([`post_phased_scroll`]) carries the phase on macOS. Linux and
/// Windows have no equivalent field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmoothScrollPhase {
    /// First output frame of a new animation.
    Began,
    /// An intermediate output frame, including frames after retargeting.
    Changed,
    /// Final frame, carrying any correction needed to reach the exact target.
    Ended,
    /// The capture source ended before the animation reached its target.
    Cancelled,
}

/// Synthesise one frame of a finite smooth-scroll animation.
///
/// On macOS wheel ticks become phaseless continuous pixel events at ten
/// points per tick, matching the line/point relationship carried in native
/// continuous events. Other platforms preserve fractional wheel ticks through
/// their native high-resolution output. Non-finite distance is rejected at
/// this I/O boundary, and zero-distance frames are dropped everywhere.
pub fn post_smooth_scroll(delta: ScrollDelta, phase: SmoothScrollPhase) {
    if !delta.is_finite() {
        return;
    }
    cfg_select! {
        target_os = "macos" => {
            macos::post_smooth_scroll(delta, phase);
        }
        _ => {
            let _ = phase;
            post_scroll(delta);
        }
    }
}

/// Synthesise one frame of a phased scroll gesture.
///
/// Unlike [`post_smooth_scroll`], macOS stamps the frame's phase onto the
/// continuous event and posts zero-distance frames too, so the application
/// sees a balanced Began/Changed/Ended gesture. Use it only where a gesture is
/// the point: AppKit's swipe recognisers (e.g. Messages revealing timestamps)
/// ignore horizontal scrolls that carry no phase. Other platforms have no phase
/// field and inject the distance exactly as [`post_smooth_scroll`] does.
/// Non-finite distance is rejected at this I/O boundary.
pub fn post_phased_scroll(delta: ScrollDelta, phase: SmoothScrollPhase) {
    if !delta.is_finite() {
        return;
    }
    cfg_select! {
        target_os = "macos" => {
            macos::post_phased_scroll(delta, phase);
        }
        _ => {
            post_smooth_scroll(delta, phase);
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
    use openlogi_core::scroll::ScrollDelta;

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    use openlogi_core::binding::{HeldKeys, KeyCombo};

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    use super::{HeldKey, HeldOutput, HoldTransition};
    use super::{QuantizedScroll, ScrollQuantizer};

    /// Synthetic high-resolution input: eight eighth-ticks must total exactly
    /// one Windows/Linux wheel detent (120 raw units). This is deterministic
    /// model data, not a hardware capture.
    #[test]
    fn fractional_frames_preserve_cumulative_wheel_distance() {
        let mut quantizer = ScrollQuantizer::default();
        let total = (0..8)
            .map(|_| {
                quantizer
                    .quantize(ScrollDelta::wheel_ticks(0.0, 0.125), 120.0)
                    .y
            })
            .sum::<i32>();
        assert_eq!(total, 120);
    }

    #[test]
    fn opposing_fractional_input_cancels_without_rounding_drift() {
        let mut quantizer = ScrollQuantizer::default();
        let forward = quantizer.quantize(ScrollDelta::wheel_ticks(0.25, 0.0), 120.0);
        let backward = quantizer.quantize(ScrollDelta::wheel_ticks(-0.25, 0.0), 120.0);
        assert_eq!(forward, QuantizedScroll { x: 30, y: 0 });
        assert_eq!(backward, QuantizedScroll { x: -30, y: 0 });
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    fn combo(label: &str) -> KeyCombo {
        label.parse().expect("test shortcut must be valid")
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    fn whole(combo: &KeyCombo) -> super::HeldChordKind {
        super::HeldChordKind::Chord(combo.clone(), HeldKeys::WholeChord)
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    fn tapping(combo: &KeyCombo) -> super::HeldChordKind {
        super::HeldChordKind::Chord(combo.clone(), HeldKeys::ModifiersOnly)
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn shared_control_stays_down_until_its_last_chord_ends() {
        let control_a = combo("Ctrl+A");
        let control_b = combo("Ctrl+B");
        let mut output = HeldOutput::default();

        assert_eq!(
            output.transition(None, Some(&whole(&control_a))),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Control, HeldKey::Key(control_a.key())],
                tap: None,
            }
        );
        assert_eq!(
            output.transition(None, Some(&whole(&control_b))),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Key(control_b.key())],
                tap: None,
            }
        );
        assert_eq!(
            output.transition(Some(&whole(&control_a)), None),
            HoldTransition {
                up: vec![HeldKey::Key(control_a.key())],
                down: vec![],
                tap: None,
            }
        );
        assert_eq!(
            output.transition(Some(&whole(&control_b)), None),
            HoldTransition {
                up: vec![HeldKey::Control, HeldKey::Key(control_b.key())],
                down: vec![],
                tap: None,
            }
        );
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn command_and_control_share_one_physical_output() {
        let command_a = combo("Cmd+A");
        let control_b = combo("Ctrl+B");
        let mut output = HeldOutput::default();

        output.transition(None, Some(&whole(&command_a)));
        assert_eq!(
            output.transition(None, Some(&whole(&control_b))),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Key(control_b.key())],
                tap: None,
            }
        );
        assert_eq!(
            output.transition(Some(&whole(&command_a)), None),
            HoldTransition {
                up: vec![HeldKey::Key(command_a.key())],
                down: vec![],
                tap: None,
            }
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn command_and_control_are_distinct_physical_outputs() {
        let command_a = combo("Cmd+A");
        let control_b = combo("Ctrl+B");
        let mut output = HeldOutput::default();

        output.transition(None, Some(&whole(&command_a)));
        assert_eq!(
            output.transition(None, Some(&whole(&control_b))),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Control, HeldKey::Key(control_b.key())],
                tap: None,
            }
        );
        assert_eq!(
            output.transition(Some(&whole(&command_a)), None),
            HoldTransition {
                up: vec![HeldKey::Command, HeldKey::Key(command_a.key())],
                down: vec![],
                tap: None,
            }
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn super_and_command_share_one_physical_output() {
        let super_a = combo("Super+A");
        let command_b = combo("Cmd+B");
        let mut output = HeldOutput::default();

        assert_eq!(
            output.transition(None, Some(&whole(&super_a))),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Command, HeldKey::Key(super_a.key())],
                tap: None,
            }
        );
        assert_eq!(
            output.transition(None, Some(&whole(&command_b))),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Key(command_b.key())],
                tap: None,
            }
        );
        assert_eq!(
            output.transition(Some(&whole(&super_a)), None),
            HoldTransition {
                up: vec![HeldKey::Key(super_a.key())],
                down: vec![],
                tap: None,
            }
        );
        assert_eq!(
            output.transition(Some(&whole(&command_b)), None),
            HoldTransition {
                up: vec![HeldKey::Command, HeldKey::Key(command_b.key())],
                down: vec![],
                tap: None,
            }
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn super_plus_command_owns_the_physical_key_once() {
        let chord = combo("Cmd+Super+A");
        assert_eq!(
            super::held_keys(&chord, HeldKeys::WholeChord),
            vec![HeldKey::Command, HeldKey::Key(chord.key())]
        );
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn super_is_distinct_from_control() {
        let super_a = combo("Super+A");
        let command_b = combo("Cmd+B");
        let mut output = HeldOutput::default();

        assert_eq!(
            output.transition(None, Some(&whole(&super_a))),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Super, HeldKey::Key(super_a.key())],
                tap: None,
            }
        );
        assert_eq!(
            output.transition(None, Some(&whole(&command_b))),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Control, HeldKey::Key(command_b.key())],
                tap: None,
            }
        );
        assert_eq!(
            output.transition(Some(&whole(&super_a)), None),
            HoldTransition {
                up: vec![HeldKey::Super, HeldKey::Key(super_a.key())],
                down: vec![],
                tap: None,
            }
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn shared_command_stays_down_until_its_last_chord_ends() {
        let command_a = combo("Cmd+A");
        let command_b = combo("Cmd+B");
        let mut output = HeldOutput::default();

        output.transition(None, Some(&whole(&command_a)));
        assert_eq!(
            output.transition(None, Some(&whole(&command_b))),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Key(command_b.key())],
                tap: None,
            }
        );
        assert_eq!(
            output.transition(Some(&whole(&command_a)), None),
            HoldTransition {
                up: vec![HeldKey::Key(command_a.key())],
                down: vec![],
                tap: None,
            }
        );
        assert_eq!(
            output.transition(Some(&whole(&command_b)), None),
            HoldTransition {
                up: vec![HeldKey::Command, HeldKey::Key(command_b.key())],
                down: vec![],
                tap: None,
            }
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn a_modifiers_only_hold_never_owns_its_ordinary_key() {
        let control_tab = combo("Ctrl+Tab");
        let mut output = HeldOutput::default();

        // The ordinary key is absent from both edges: it is tapped once as the
        // hold starts, not held, so it never enters the ownership map.
        assert_eq!(
            output.transition(None, Some(&tapping(&control_tab))),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Control],
                tap: Some(HeldKey::Key(control_tab.key())),
            }
        );
        assert_eq!(
            output.transition(Some(&tapping(&control_tab)), None),
            HoldTransition {
                up: vec![HeldKey::Control],
                down: vec![],
                tap: None,
            }
        );
    }

    /// A whole-chord hold of the same chord must keep holding the ordinary
    /// key: this is the difference the two actions exist to express.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn a_whole_chord_hold_of_the_same_chord_still_owns_its_ordinary_key() {
        let control_tab = combo("Ctrl+Tab");
        let mut output = HeldOutput::default();

        assert_eq!(
            output.transition(None, Some(&whole(&control_tab))),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Control, HeldKey::Key(control_tab.key())],
                tap: None,
            }
        );
    }

    /// Two buttons, one ordinary key: a whole-chord hold keeps Tab down, so a
    /// modifiers-only hold that starts meanwhile must not tap it. The tap's up
    /// edge would release Tab under the first hold, which still owns it until
    /// its own release.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn a_modifiers_only_hold_skips_its_tap_while_another_hold_owns_the_key() {
        let shift_tab = combo("Shift+Tab");
        let alt_tab = combo("Alt+Tab");
        let tab = HeldKey::Key(alt_tab.key());
        let mut output = HeldOutput::default();

        output.transition(None, Some(&whole(&shift_tab)));
        assert_eq!(
            output.transition(None, Some(&tapping(&alt_tab))),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Alt],
                tap: None,
            }
        );
        assert_eq!(
            output.transition(Some(&tapping(&alt_tab)), None),
            HoldTransition {
                up: vec![HeldKey::Alt],
                down: vec![],
                tap: None,
            }
        );
        assert_eq!(
            output.transition(Some(&whole(&shift_tab)), None),
            HoldTransition {
                up: vec![HeldKey::Shift, tab],
                down: vec![],
                tap: None,
            }
        );
        // With the owner gone, the next modifiers-only hold taps again.
        assert_eq!(
            output.transition(None, Some(&tapping(&alt_tab))),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Alt],
                tap: Some(tab),
            }
        );
    }

    /// The skip is about ownership, not about another hold merely existing:
    /// a modifiers-only hold owns no ordinary key, so a second one still taps,
    /// and a whole-chord hold that starts meanwhile still presses and owns Tab.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn a_modifiers_only_hold_never_blocks_another_hold_of_the_same_key() {
        let alt_tab = combo("Alt+Tab");
        let shift_tab = combo("Shift+Tab");
        let tab = HeldKey::Key(alt_tab.key());
        let mut output = HeldOutput::default();

        output.transition(None, Some(&tapping(&alt_tab)));
        assert_eq!(
            output.transition(None, Some(&tapping(&shift_tab))),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Shift],
                tap: Some(tab),
            }
        );
        output.transition(Some(&tapping(&shift_tab)), None);
        assert_eq!(
            output.transition(None, Some(&whole(&shift_tab))),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Shift, tab],
                tap: None,
            }
        );
        assert_eq!(
            output.transition(Some(&whole(&shift_tab)), None),
            HoldTransition {
                up: vec![HeldKey::Shift, tab],
                down: vec![],
                tap: None,
            }
        );
    }

    /// A press whose whole-chord hold is replaced by a modifiers-only one of
    /// the same key releases the key first, so the replacement owns no
    /// conflict and taps as a fresh hold does.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn replacing_a_whole_chord_hold_frees_the_key_for_the_tap() {
        let shift_tab = combo("Shift+Tab");
        let alt_tab = combo("Alt+Tab");
        let tab = HeldKey::Key(alt_tab.key());
        let mut output = HeldOutput::default();

        output.transition(None, Some(&whole(&shift_tab)));
        assert_eq!(
            output.transition(Some(&whole(&shift_tab)), Some(&tapping(&alt_tab))),
            HoldTransition {
                up: vec![HeldKey::Shift, tab],
                down: vec![HeldKey::Alt],
                tap: Some(tab),
            }
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn replacement_preserves_shared_physical_outputs() {
        let old = combo("Ctrl+A");
        let new = combo("Ctrl+B");
        let mut output = HeldOutput::default();

        output.transition(None, Some(&whole(&old)));
        assert_eq!(
            output.transition(Some(&whole(&old)), Some(&whole(&new))),
            HoldTransition {
                up: vec![HeldKey::Key(old.key())],
                down: vec![HeldKey::Key(new.key())],
                tap: None,
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

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn modifier_only_hold_shares_a_modifier_and_owns_no_ordinary_key() {
        use super::{HeldChordKind, Modifiers};

        let control_a = combo("Ctrl+A");
        let just_ctrl: Modifiers = "Ctrl".parse().expect("valid modifier");
        let mod_input = HeldChordKind::Modifiers(just_ctrl);
        let mut output = HeldOutput::default();

        assert_eq!(
            output.transition(None, Some(&whole(&control_a))),
            HoldTransition {
                up: vec![],
                down: vec![HeldKey::Control, HeldKey::Key(control_a.key())],
                tap: None,
            }
        );

        // Ctrl is already owned by the chord: the modifier hold adds no edge.
        assert_eq!(
            output.transition(None, Some(&mod_input)),
            HoldTransition {
                up: vec![],
                down: vec![],
                tap: None,
            }
        );

        // Releasing the modifier hold must not disturb the chord's Ctrl or key.
        assert_eq!(
            output.transition(Some(&mod_input), None),
            HoldTransition {
                up: vec![],
                down: vec![],
                tap: None,
            }
        );

        assert_eq!(
            output.transition(Some(&whole(&control_a)), None),
            HoldTransition {
                up: vec![HeldKey::Control, HeldKey::Key(control_a.key())],
                down: vec![],
                tap: None,
            }
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn only_bare_modifier_holds_reach_the_tap_mirror() {
        use super::{HeldChordKind, Modifiers};

        let control_a = combo("Ctrl+A");
        let just_ctrl: Modifiers = "Ctrl".parse().expect("valid modifier");
        let mod_input = HeldChordKind::Modifiers(just_ctrl);
        let mut output = HeldOutput::default();

        // A chord's Ctrl is synthesised but must not mark the mirror.
        output.transition(None, Some(&whole(&control_a)));
        assert!(!output.modifier_hold_modifiers().contains(HeldKey::Control));

        output.transition(None, Some(&mod_input));
        assert!(output.modifier_hold_modifiers().contains(HeldKey::Control));

        // Releasing the bare hold clears the mirror even while the chord
        // still owns the physical Ctrl output.
        output.transition(Some(&mod_input), None);
        assert!(!output.modifier_hold_modifiers().contains(HeldKey::Control));
        assert!(output.owners.contains_key(&HeldKey::Control));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn concurrent_bare_modifier_holds_reference_count_the_tap_mirror() {
        use super::{HeldChordKind, HeldModifiers, Modifiers};

        let ctrl: Modifiers = "Ctrl".parse().expect("valid modifier");
        let ctrl_shift: Modifiers = "Ctrl+Shift".parse().expect("valid modifier set");
        let first = HeldChordKind::Modifiers(ctrl);
        let second = HeldChordKind::Modifiers(ctrl_shift);
        let mut output = HeldOutput::default();

        output.transition(None, Some(&first));
        output.transition(None, Some(&second));
        output.transition(Some(&first), None);

        // Ctrl is still owned by the second hold; Shift too.
        let mirror = output.modifier_hold_modifiers();
        assert!(mirror.contains(HeldKey::Control));
        assert!(mirror.contains(HeldKey::Shift));

        output.transition(Some(&second), None);
        assert_eq!(output.modifier_hold_modifiers(), HeldModifiers::default());
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn modifier_only_hold_never_emits_an_ordinary_key_edge() {
        use super::{HeldChordKind, Modifiers};

        let mods: Modifiers = "Ctrl+Shift+Alt".parse().expect("valid modifier set");
        let transition =
            HeldOutput::default().transition(None, Some(&HeldChordKind::Modifiers(mods)));

        assert!(
            !transition.down.is_empty(),
            "a modifier hold must emit edges"
        );
        for key in &transition.down {
            assert!(
                !matches!(key, HeldKey::Key(_)),
                "modifier-only hold must not press an ordinary key: {key:?}"
            );
        }
    }
}
