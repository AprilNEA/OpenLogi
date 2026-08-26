//! A platform-neutral synthesis IR.
//!
//! [`Action`] has one variant per user-facing behaviour (52 of them), but the
//! three `openlogi-inject` backends don't care about most of that
//! granularity — they care about *mechanism*: "press this chord", "click
//! this mouse button", "fire this media key", "there is no portable way to
//! do this, use the OS-specific path". [`Action::effect`] is the single
//! exhaustive match that sorts every variant into one of those buckets, so
//! each backend matches on ~10-variant [`Effect`] instead of re-deriving the
//! full `Action` vocabulary three times.
//!
//! [`Shortcut`], [`MediaKey`], and [`NativeAction`] are semantic, not
//! mechanical: the same named shortcut is not the same chord on every OS
//! (`BrowserBack` is ⌘\[ on macOS, Alt+Left on Linux, and a dedicated
//! virtual key with no modifier on Windows), so each backend owns its own
//! lookup table from these enums to its platform's key/API. The
//! exhaustiveness check on those enums is what keeps all three backends
//! honest when a variant is added — a backend that forgets a case fails to
//! compile rather than silently no-op-ing.

use super::action::{Action, WorkflowStep};
use super::key_combo::KeyCombo;

/// What firing an [`Action`] should do, independent of platform.
///
/// `openlogi_inject`'s per-OS backends match on this instead of on
/// [`Action`] directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect<'a> {
    /// Suppress the input entirely: no OS event at all.
    None,
    /// Synthesise a physical mouse button click.
    Click(MouseButton),
    /// Fire a named shortcut. Each backend maps this to its own platform
    /// chord (or, for the rare shortcut with no chord at all on some OS, a
    /// dedicated native code path) — see that backend's `combo` table.
    Shortcut(Shortcut),
    /// Press an already-resolved keyboard chord: a user-recorded
    /// [`Action::CustomShortcut`], or a workflow's `PressKey` step.
    Key(&'a KeyCombo),
    /// A user-recorded chord whose output is held by a lifecycle-aware
    /// runtime. A one-shot executor treats this as [`Effect::Key`] so direct
    /// dispatch remains balanced when no matching release can arrive.
    HeldKey(&'a KeyCombo),
    /// Synthesise one scroll tick. `dx`/`dy` are unit direction (-1/0/1);
    /// each backend applies its own tick magnitude.
    Scroll {
        /// Horizontal direction: -1 left, 1 right, 0 none.
        dx: i8,
        /// Vertical direction: -1 down, 1 up, 0 none.
        dy: i8,
    },
    /// Fire a media/volume key. Every backend reaches these through a
    /// dedicated OS mechanism rather than an ordinary keyboard chord.
    Media(MediaKey),
    /// A window-manager or power action with no shared cross-platform
    /// chord — each backend has its own dedicated handling, which may be a
    /// debug-logged no-op where the OS has no equivalent at all.
    Native(NativeAction),
    /// A power-user scripting escape hatch.
    Script(Script<'a>),
    /// Type this text via unicode input.
    Text(&'a str),
    /// Handled entirely by the agent/hook layer — DPI presets, SmartShift,
    /// the Actions Ring, and launching an application. The injector logs
    /// and does nothing.
    ///
    /// [`Action::OpenApplication`] is included here even though
    /// `openlogi_inject::execute` does open it: that happens in the
    /// platform-independent dispatcher *before* a backend ever sees the
    /// action (the config target is opened via the `opener` crate, not
    /// through any per-OS synthesis path), so from a backend's point of
    /// view it is exactly as much a no-op as the DPI actions.
    AgentSide,
}

/// A physical mouse button an [`Effect::Click`] should press.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// Primary button.
    Left,
    /// Secondary button.
    Right,
    /// Wheel-click button.
    Middle,
    /// Extra "back" side button (button 4).
    Back,
    /// Extra "forward" side button (button 5).
    Forward,
}

/// A named shortcut whose chord is the same *concept* on every OS but not
/// the same keys.
///
/// Each backend owns a `Shortcut -> KeyCombo` table (plus, for the couple of
/// shortcuts a given OS has no ordinary chord for, a small override) rather
/// than sharing one table — see the per-backend `combo`/`press_shortcut`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, strum::VariantArray)]
pub enum Shortcut {
    /// Copy the selection.
    Copy,
    /// Paste from the clipboard.
    Paste,
    /// Cut the selection.
    Cut,
    /// Undo the last action.
    Undo,
    /// Redo the last undone action.
    Redo,
    /// Select all content.
    SelectAll,
    /// Open find/search.
    Find,
    /// Save the current document.
    Save,
    /// Navigate backward in browser history.
    BrowserBack,
    /// Navigate forward in browser history.
    BrowserForward,
    /// Open a new tab.
    NewTab,
    /// Close the current tab.
    CloseTab,
    /// Reopen the last closed tab.
    ReopenTab,
    /// Switch to the next tab.
    NextTab,
    /// Switch to the previous tab.
    PrevTab,
    /// Reload the current page.
    ReloadPage,
}

impl Shortcut {
    /// Every named shortcut, in declaration order.
    ///
    /// `#[derive(strum::VariantArray)]` generates this straight from the
    /// enum's variant list at compile time, so it cannot go stale the way a
    /// hand-written array literal could: there is no second, independently
    /// editable list for it to drift from — a variant that's missing here
    /// would mean the enum itself doesn't have it. The single shared
    /// iteration source for each backend's `Shortcut -> KeyCombo`
    /// table-completeness test.
    pub const ALL: &'static [Shortcut] = <Shortcut as strum::VariantArray>::VARIANTS;
}

/// A media/volume key.
///
/// Every backend reaches these through a dedicated OS mechanism — NX
/// system-defined keys on macOS, MPRIS/XF86 keys on Linux, dedicated media
/// virtual keys on Windows — rather than an ordinary keyboard chord.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKey {
    /// Toggle play/pause.
    PlayPause,
    /// Skip to the next track.
    NextTrack,
    /// Go back to the previous track.
    PrevTrack,
    /// Increase system volume.
    VolumeUp,
    /// Decrease system volume.
    VolumeDown,
    /// Toggle system mute.
    Mute,
}

/// A window-manager or power action with no shared cross-platform chord.
///
/// Each backend reaches it through its own dedicated OS API — or, where the
/// OS has no equivalent at all, a debug-logged no-op.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NativeAction {
    /// Show all windows across spaces (macOS Mission Control).
    MissionControl,
    /// Show all windows of the frontmost app (macOS App Exposé).
    AppExpose,
    /// Switch to the previous desktop/space.
    PreviousDesktop,
    /// Switch to the next desktop/space.
    NextDesktop,
    /// Hide all windows to reveal the desktop.
    ShowDesktop,
    /// Open the application launcher.
    LaunchpadShow,
    /// Lock the screen.
    LockScreen,
    /// Capture a full-screen screenshot.
    Screenshot,
    /// Capture a selected screen region.
    CaptureRegion,
    /// Put the computer to sleep.
    Sleep,
}

/// A power-user scripting escape hatch, borrowed from the originating
/// [`Action::RunAppleScript`], [`Action::RunShellCommand`], or
/// [`Action::Workflow`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Script<'a> {
    /// Run an AppleScript source string. macOS-only; other backends warn.
    AppleScript(&'a str),
    /// Run a shell command string.
    ShellCommand(&'a str),
    /// Run an ordered sequence of workflow steps.
    Workflow(&'a [WorkflowStep]),
}

impl Action {
    /// Classify this action into the platform-neutral [`Effect`] IR that
    /// `openlogi-inject`'s backends dispatch on.
    ///
    /// This is the single exhaustive match over the full [`Action`]
    /// vocabulary that a new backend needs — see the module docs.
    #[must_use]
    pub fn effect(&self) -> Effect<'_> {
        match self {
            Action::None => Effect::None,

            Action::LeftClick => Effect::Click(MouseButton::Left),
            Action::RightClick => Effect::Click(MouseButton::Right),
            Action::MiddleClick => Effect::Click(MouseButton::Middle),
            Action::MouseBack => Effect::Click(MouseButton::Back),
            Action::MouseForward => Effect::Click(MouseButton::Forward),

            Action::Copy => Effect::Shortcut(Shortcut::Copy),
            Action::Paste => Effect::Shortcut(Shortcut::Paste),
            Action::Cut => Effect::Shortcut(Shortcut::Cut),
            Action::Undo => Effect::Shortcut(Shortcut::Undo),
            Action::Redo => Effect::Shortcut(Shortcut::Redo),
            Action::SelectAll => Effect::Shortcut(Shortcut::SelectAll),
            Action::Find => Effect::Shortcut(Shortcut::Find),
            Action::Save => Effect::Shortcut(Shortcut::Save),

            Action::BrowserBack => Effect::Shortcut(Shortcut::BrowserBack),
            Action::BrowserForward => Effect::Shortcut(Shortcut::BrowserForward),
            Action::NewTab => Effect::Shortcut(Shortcut::NewTab),
            Action::CloseTab => Effect::Shortcut(Shortcut::CloseTab),
            Action::ReopenTab => Effect::Shortcut(Shortcut::ReopenTab),
            Action::NextTab => Effect::Shortcut(Shortcut::NextTab),
            Action::PrevTab => Effect::Shortcut(Shortcut::PrevTab),
            Action::ReloadPage => Effect::Shortcut(Shortcut::ReloadPage),

            Action::MissionControl => Effect::Native(NativeAction::MissionControl),
            Action::AppExpose => Effect::Native(NativeAction::AppExpose),
            Action::PreviousDesktop => Effect::Native(NativeAction::PreviousDesktop),
            Action::NextDesktop => Effect::Native(NativeAction::NextDesktop),
            Action::ShowDesktop => Effect::Native(NativeAction::ShowDesktop),
            Action::LaunchpadShow => Effect::Native(NativeAction::LaunchpadShow),

            Action::LockScreen => Effect::Native(NativeAction::LockScreen),
            Action::Screenshot => Effect::Native(NativeAction::Screenshot),
            Action::CaptureRegion => Effect::Native(NativeAction::CaptureRegion),
            Action::Sleep => Effect::Native(NativeAction::Sleep),

            Action::PlayPause => Effect::Media(MediaKey::PlayPause),
            Action::NextTrack => Effect::Media(MediaKey::NextTrack),
            Action::PrevTrack => Effect::Media(MediaKey::PrevTrack),
            Action::VolumeUp => Effect::Media(MediaKey::VolumeUp),
            Action::VolumeDown => Effect::Media(MediaKey::VolumeDown),
            Action::MuteVolume => Effect::Media(MediaKey::Mute),

            // DPI/SmartShift/the Actions Ring/OpenApplication are all handled
            // above (or beside) the injector — see `Effect::AgentSide`.
            Action::CycleDpiPresets
            | Action::SetDpiPreset(_)
            | Action::ToggleSmartShift
            | Action::ShowActionsRing
            | Action::OpenApplication(_) => Effect::AgentSide,

            Action::ScrollUp => Effect::Scroll { dx: 0, dy: 1 },
            Action::ScrollDown => Effect::Scroll { dx: 0, dy: -1 },
            Action::HorizontalScrollLeft => Effect::Scroll { dx: -1, dy: 0 },
            Action::HorizontalScrollRight => Effect::Scroll { dx: 1, dy: 0 },

            Action::CustomShortcut(combo) => Effect::Key(combo),
            Action::HoldShortcut(combo) => Effect::HeldKey(combo),

            Action::TypeText(text) => Effect::Text(text),
            Action::RunAppleScript(src) => Effect::Script(Script::AppleScript(src)),
            Action::RunShellCommand(cmd) => Effect::Script(Script::ShellCommand(cmd)),
            Action::Workflow(steps) => Effect::Script(Script::Workflow(steps)),
        }
    }
}
