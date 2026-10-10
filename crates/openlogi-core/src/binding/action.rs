//! The action vocabulary a button can bind to, plus workflow steps.

use serde::{Deserialize, Serialize};

use super::application_target::ApplicationTarget;
use super::category::Category;
use super::key_combo::KeyCombo;

/// What pressing a [`ButtonId`](crate::binding::ButtonId) should do.
///
/// Serialization uses serde's default external tagging: unit variants
/// serialize as a bare string (`"BrowserBack"`) and the tuple variant
/// serializes as a single-key table (`{ CustomShortcut = "my chord" }`).
///
/// **Stability contract:** existing variant *names* are frozen — they form the
/// on-disk `config.toml` schema. New variants may be appended freely; removing
/// or renaming a variant requires a `schema_version` bump and a migration.
///
/// This type is pure config data: OS-level event synthesis for each variant
/// lives in the `openlogi-inject` crate (`openlogi_inject::execute`), keeping
/// this crate platform- and IO-free.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Action {
    // ── System ───────────────────────────────────────────────────────────────
    /// Suppress the input entirely — the button or wheel direction is captured
    /// but no OS event is synthesised, so the physical input does nothing.
    None,

    // ── Mouse ────────────────────────────────────────────────────────────────
    /// Primary mouse button.
    LeftClick,
    /// Secondary mouse button.
    RightClick,
    /// Middle mouse button (wheel click).
    MiddleClick,
    /// Mouse "back" side button (extra button 4). Synthesizes the real mouse
    /// button event, which browsers and most apps interpret as "navigate back"
    /// natively — unlike [`Action::BrowserBack`], which sends ⌘[ and is ignored
    /// by many apps.
    MouseBack,
    /// Mouse "forward" side button (extra button 5). Native counterpart to
    /// [`Action::MouseBack`]; see [`Action::BrowserForward`] for the ⌘] form.
    MouseForward,

    // ── Editing ──────────────────────────────────────────────────────────────
    /// Copy the current selection (⌘C / Ctrl+C).
    Copy,
    /// Paste from the clipboard (⌘V / Ctrl+V).
    Paste,
    /// Cut the current selection (⌘X / Ctrl+X).
    Cut,
    /// Undo the last action (⌘Z / Ctrl+Z).
    Undo,
    /// Redo the last undone action (⌘⇧Z on macOS / Ctrl+Shift+Z on Linux).
    ///
    /// Note: Ctrl+Y is the dominant redo shortcut in LibreOffice and many GTK
    /// apps. Ctrl+Shift+Z is used here because it mirrors the macOS convention
    /// and works in GNOME text fields, browsers, and Electron apps. If Ctrl+Y
    /// coverage is needed, a `CustomShortcut` binding is the escape hatch.
    Redo,
    /// Select all content (⌘A / Ctrl+A).
    SelectAll,
    /// Open the find / search bar (⌘F / Ctrl+F).
    Find,
    /// Save the current document (⌘S / Ctrl+S).
    Save,

    // ── Browser / Navigation ──────────────────────────────────────────────────
    /// Navigate backward in browser history.
    BrowserBack,
    /// Navigate forward in browser history.
    BrowserForward,
    /// Open a new tab (⌘T / Ctrl+T).
    NewTab,
    /// Close the current tab (⌘W / Ctrl+W).
    CloseTab,
    /// Reopen the last closed tab (⌘⇧T / Ctrl+Shift+T).
    ReopenTab,
    /// Switch to the next tab (⌃⇥ / Ctrl+Tab).
    NextTab,
    /// Switch to the previous tab (⌃⇧⇥ / Ctrl+Shift+Tab).
    PrevTab,
    /// Reload the current page (⌘R / Ctrl+R).
    ReloadPage,

    // ── Navigation / Window ───────────────────────────────────────────────────
    /// macOS Mission Control (⌃↑).
    MissionControl,
    /// macOS App Exposé — all windows for the current app (⌃↓).
    AppExpose,
    /// Switch to the previous desktop / Space.
    PreviousDesktop,
    /// Switch to the next desktop / Space.
    NextDesktop,
    /// Show the desktop (hide all windows).
    ShowDesktop,
    /// Open Launchpad.
    LaunchpadShow,

    // ── System ────────────────────────────────────────────────────────────────
    /// Lock the screen (⌘⌃Q on macOS).
    ///
    /// On Linux, calls `org.freedesktop.login1.Manager.LockSession($XDG_SESSION_ID)`
    /// on the system bus (current session only). Falls back to Super+L when
    /// `$XDG_SESSION_ID` is unset or on non-systemd systems.
    LockScreen,
    /// Capture a screenshot.
    Screenshot,
    /// Capture a selected screen region to the clipboard.
    ///
    /// macOS uses Cmd+Shift+Ctrl+4; Windows uses Win+Shift+S. Linux delegates
    /// to the desktop environment's screenshot handler via Print Screen.
    CaptureRegion,

    // ── Media ────────────────────────────────────────────────────────────────
    /// Toggle media play/pause.
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
    MuteVolume,

    // ── DPI ──────────────────────────────────────────────────────────────────
    /// Step through the configured DPI preset list (P1.7).
    CycleDpiPresets,
    /// Jump to a specific zero-based preset in the device's DPI preset list.
    /// Out-of-range indices clamp to the list length at fire time (P1.7).
    SetDpiPreset(u8),
    /// Toggle the HID++ SmartShift ratchet/free-spin wheel mode (P1.1).
    ToggleSmartShift,

    // ── Scroll ───────────────────────────────────────────────────────────────
    /// Synthesise a vertical scroll-up tick.
    ScrollUp,
    /// Synthesise a vertical scroll-down tick.
    ScrollDown,
    /// Synthesise a horizontal scroll-left tick.
    HorizontalScrollLeft,
    /// Synthesise a horizontal scroll-right tick.
    HorizontalScrollRight,

    // ── Custom ───────────────────────────────────────────────────────────────
    /// Replay an arbitrary recorded key chord (P1.3).
    ///
    /// Holds the structured chord data so `openlogi_inject::execute` can post the
    /// real keystroke (macOS: CGEventPost with the encoded modifier flags).
    /// The `display` field is used by [`Action::label`] so the popover
    /// shows the user-friendly chord name.
    CustomShortcut(KeyCombo),

    // ── System (appended) ────────────────────────────────────────────────────
    /// Put the computer to sleep. Appended after `CustomShortcut` because the
    /// serde variant index is the wire format (see the stability contract
    /// above) — new variants only ever go at the end.
    Sleep,
    /// Type an arbitrary string by emitting unicode characters (macOS
    /// `CGEventKeyboardSetUnicodeString`). Used for macro text. Power-user
    /// escape hatch — excluded from the default catalog.
    TypeText(String),
    /// Run an AppleScript via `osascript -e <source>`. Power-user escape hatch.
    RunAppleScript(String),
    /// Run a shell command via `/bin/sh -c <command>`. Power-user escape hatch.
    RunShellCommand(String),
    /// Run a timed, ordered sequence of steps — the native, no-code version of
    /// "type 'bite me', wait 5s, press Enter, wait 5s, type more, Escape". Each
    /// step is one of the power-user actions or a `Delay`. The sequencer
    /// (`openlogi-inject`) runs them in order, awaiting `Delay`s. Power-user
    /// escape hatch — excluded from the default catalog.
    Workflow(Vec<WorkflowStep>),
    /// Open the configured Actions Ring at the current pointer position.
    /// The agent handles the ring session rather than the OS injector.
    ShowActionsRing,
    /// Open an application, folder, filesystem path, or platform URL.
    OpenApplication(ApplicationTarget),
    /// Hold an arbitrary recorded key chord for the lifetime of its physical
    /// button press. This is the push-to-talk counterpart to
    /// [`Action::CustomShortcut`], which emits an immediate down/up pair.
    ///
    /// Lifecycle-aware runtimes emit the chord's down edge when the press
    /// starts and its up edge for every terminal outcome, including capture
    /// cancellation and shutdown. Dispatchers without a release context must
    /// degrade this action to a balanced tap rather than leave keys held.
    HoldShortcut(KeyCombo),
}

/// One step in a [`Action::Workflow`]. A workflow is a `Vec<WorkflowStep>`
/// executed in order by the inject layer; `Delay` introduces a pause between
/// the surrounding steps.
///
/// `PressKey` reuses [`KeyCombo`] (the same model as [`Action::CustomShortcut`])
/// so a step can press a key chord. The other variants mirror their standalone
/// [`Action`] counterparts.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WorkflowStep {
    /// Type a unicode string (see [`Action::TypeText`]).
    TypeText(String),
    /// Press a key chord (see [`Action::CustomShortcut`] / [`KeyCombo`]).
    PressKey(KeyCombo),
    /// Wait `millis` milliseconds before the next step.
    Delay {
        /// Pause length in milliseconds.
        millis: u64,
    },
    /// Run an AppleScript (see [`Action::RunAppleScript`]).
    RunAppleScript(String),
    /// Run a shell command (see [`Action::RunShellCommand`]).
    RunShellCommand(String),
}

/// X-macro table of every payload-free [`Action`] variant.
///
/// Each row is `Variant "Label" "i18n.key" Category Icon`, optionally followed
/// by `not_pickable` for a row [`Action::catalog`] must omit. This is the single
/// place a plain action is declared; payload-carrying variants (`SetDpiPreset`,
/// `CustomShortcut`, …) build their label/category/icon from their payload and
/// keep hand-written arms alongside the generated ones instead.
///
/// `macro_rules!` can only emit items into the module it is invoked from, so
/// this table doesn't generate code itself — it forwards its rows verbatim to
/// a `$callback!` macro chosen by the caller. `action.rs` (below) uses it to
/// derive [`Action::label`], [`Action::category`], and [`Action::catalog`];
/// `action_ring::icon` uses it to derive [`ActionRingIcon::for_action`](
/// super::action_ring::ActionRingIcon::for_action). Row order is
/// [`Action::catalog`]'s output order, grouped by category to match the
/// popover section layout — edit rows here only, never in a callback's match.
macro_rules! for_each_unit_action {
    ($callback:ident) => {
        $callback! {
            // Mouse
            LeftClick "Left Click" "actions.left_click" Mouse Pointer,
            RightClick "Right Click" "actions.right_click" Mouse Pointer,
            MiddleClick "Middle Click" "actions.middle_click" Mouse Mouse,
            MouseBack "Back (Button 4)" "actions.back_button_4" Mouse MouseBack,
            MouseForward "Forward (Button 5)" "actions.forward_button_5" Mouse MouseForward,
            // Editing
            Copy "Copy" "common.copy" Editing Copy,
            Paste "Paste" "common.paste" Editing Paste,
            Cut "Cut" "common.cut" Editing Cut,
            Undo "Undo" "common.undo" Editing Undo,
            Redo "Redo" "common.redo" Editing Redo,
            SelectAll "Select All" "common.select_all" Editing SelectAll,
            Find "Find" "actions.find" Editing Search,
            Save "Save" "common.save" Editing Save,
            // Browser
            BrowserBack "Browser Back" "actions.browser_back" Browser ArrowLeft,
            BrowserForward "Browser Forward" "actions.browser_forward" Browser ArrowRight,
            NewTab "New Tab" "actions.new_tab" Browser NewTab,
            CloseTab "Close Tab" "actions.close_tab" Browser CloseTab,
            ReopenTab "Reopen Tab" "actions.reopen_tab" Browser ReopenTab,
            NextTab "Next Tab" "actions.next_tab" Browser NextTab,
            PrevTab "Previous Tab" "actions.previous_tab" Browser PreviousTab,
            ReloadPage "Reload Page" "actions.reload_page" Browser Reload,
            // Navigation
            MissionControl "Mission Control" "actions.mission_control" Navigation Grid,
            AppExpose "App Exposé" "actions.app_expose" Navigation Layers,
            PreviousDesktop "Previous Desktop" "actions.previous_desktop" Navigation PreviousDesktop,
            NextDesktop "Next Desktop" "actions.next_desktop" Navigation NextDesktop,
            ShowDesktop "Show Desktop" "actions.show_desktop" Navigation Monitor,
            LaunchpadShow "Launchpad" "actions.launchpad" Navigation Applications,
            // System
            None "Do Nothing" "pointer.do_nothing" System Ban,
            LockScreen "Lock Screen" "actions.lock_screen" System Lock,
            Screenshot "Screenshot" "actions.screenshot" System Camera,
            CaptureRegion "Capture Region" "actions.capture_region" System Camera,
            Sleep "Sleep" "actions.sleep" System Monitor,
            ShowActionsRing "Actions Ring" "action_ring.actions_ring" System Grid,
            // Media
            PlayPause "Play / Pause" "actions.play_pause" Media Play,
            NextTrack "Next Track" "actions.next_track" Media NextTrack,
            PrevTrack "Previous Track" "actions.previous_track" Media PreviousTrack,
            VolumeUp "Volume Up" "actions.volume_up" Media Volume,
            VolumeDown "Volume Down" "actions.volume_down" Media VolumeDown,
            MuteVolume "Mute" "actions.mute" Media Mute,
            // DPI
            CycleDpiPresets "Cycle DPI Presets" "pointer.cycle_dpi_presets" Dpi Gauge,
            ToggleSmartShift "Toggle SmartShift" "pointer.toggle_smartshift" Dpi Refresh,
            // Scroll
            ScrollUp "Scroll Up" "actions.scroll_up" Scroll ArrowUp,
            ScrollDown "Scroll Down" "actions.scroll_down" Scroll ArrowDown,
            HorizontalScrollLeft "Scroll Left" "actions.scroll_left" Scroll ScrollLeft,
            HorizontalScrollRight "Scroll Right" "actions.scroll_right" Scroll ScrollRight,
        }
    };
}
pub(super) use for_each_unit_action;

/// Builds `label`, `translation_key`, `category`, and `catalog` from
/// [`for_each_unit_action!`]'s rows, splicing in the hand-written arms for
/// payload-carrying variants so each generated `match` still covers every
/// [`Action`] variant exhaustively.
macro_rules! derive_action_core {
    ( $( $variant:ident $label:literal $translation_key:literal $category:ident $icon:ident $( $tag:ident )? ),* $(,)? ) => {
        impl Action {
            /// Display label for the popover row.
            ///
            /// Returns `String` rather than `&str` so parameterized variants (e.g.
            /// `SetDpiPreset(i)`, `CustomShortcut(s)`) can build a label that
            /// includes their payload.
            #[must_use]
            pub fn label(&self) -> String {
                match self {
                    $( Action::$variant => $label.into(), )*
                    Action::SetDpiPreset(i) => format!("DPI Preset {}", i + 1),
                    Action::CustomShortcut(combo) => combo.rendered_label(),
                    Action::TypeText(s) => format!("Type \"{s}\""),
                    Action::RunAppleScript(_) => "Run AppleScript".into(),
                    Action::RunShellCommand(_) => "Run Command".into(),
                    Action::Workflow(steps) => format!("Workflow ({} steps)", steps.len()),
                    Action::OpenApplication(target) => format!("Open {}", target.display_name()),
                    Action::HoldShortcut(combo) => format!("Hold {}", combo.rendered_label()),
                }
            }

            /// Stable catalog key for a payload-free action label.
            ///
            /// Payload-carrying actions return `None`: their display text needs
            /// interpolation or contains user data and cannot be represented by
            /// a key alone.
            #[must_use]
            pub fn translation_key(&self) -> Option<&'static str> {
                match self {
                    $( Action::$variant => Some($translation_key), )*
                    Action::SetDpiPreset(_)
                    | Action::CustomShortcut(_)
                    | Action::TypeText(_)
                    | Action::RunAppleScript(_)
                    | Action::RunShellCommand(_)
                    | Action::Workflow(_)
                    | Action::OpenApplication(_)
                    | Action::HoldShortcut(_) => None,
                }
            }

            /// Resolve the stable key for a payload-free English action label.
            ///
            /// The Actions Ring wire contract carries this English label for
            /// compatibility with older helpers. The overlay converts it at
            /// the localization boundary rather than changing the wire value.
            #[must_use]
            pub fn translation_key_for_label(label: &str) -> Option<&'static str> {
                match label {
                    $( $label => Some($translation_key), )*
                    _ => None,
                }
            }

            /// Which [`Category`] this action belongs to, used for popover grouping.
            #[must_use]
            pub fn category(&self) -> Category {
                match self {
                    $( Action::$variant => Category::$category, )*
                    // CustomShortcut is assigned to Editing so it doesn't need a
                    // separate arm (it's not in the picker catalog).
                    Action::CustomShortcut(_)
                    | Action::TypeText(_)
                    | Action::RunAppleScript(_)
                    | Action::RunShellCommand(_)
                    | Action::Workflow(_)
                    | Action::HoldShortcut(_) => Category::Editing,
                    Action::SetDpiPreset(_) => Category::Dpi,
                    Action::OpenApplication(_) => Category::System,
                }
            }

            /// All pickable actions in a deterministic order.
            ///
            /// [`Action::CustomShortcut`] is intentionally excluded — it is opened via
            /// "Record shortcut…" (P1.3), not selected from the catalog. Table rows
            /// tagged `not_pickable` are excluded from the catalog the same way.
            #[must_use]
            pub fn catalog() -> Vec<Action> {
                [ $( derive_action_core!(@item $variant $( $tag )?) ),* ]
                    .into_iter()
                    .flatten()
                    .collect()
            }
        }
    };
    (@item $variant:ident) => {
        Some(Action::$variant)
    };
    (@item $variant:ident not_pickable) => {
        None
    };
}

for_each_unit_action!(derive_action_core);

impl Action {
    /// The chord whose output must remain down until the originating press
    /// ends, or `None` for an instantaneous action.
    #[must_use]
    pub fn held_combo(&self) -> Option<&KeyCombo> {
        match self {
            Self::HoldShortcut(combo) => Some(combo),
            _ => None,
        }
    }
}
