# `platform/` — macOS native FFI

This directory is OpenLogi's macOS-native surface. The Objective-C FFI here runs
on **`objc2`** (0.6 / framework crates 0.3): `Retained<T>` smart pointers, typed
AppKit objects, `define_class!` for subclasses. The whole workspace's ObjC-runtime
FFI is exactly these files — keep them in sync:

- `status_item.rs` — safe `objc2` wrappers over `NSStatusItem` / `NSMenu` / `NSMenuItem`.
- `tray.rs` — the OpenLogi menu-bar semantics + the `OpenLogiMenuTarget` (`define_class!`).
- `crates/openlogi-permissions/src/lib.rs` — `CBCentralManager.authorization` (`objc2` class
  lookup) + `IOHIDCheckAccess` (`objc2-io-kit`). Its own crate because permission *status* is
  toolkit-free and shared, not a property of the settings window; bound by every rule here.
- `crates/openlogi-hook/src/macos.rs` — CGEventTap (on `core-graphics`, see below), the
  `NSWorkspace` frontmost-app read (`objc2`), and the Accessibility-trust check/prompt
  (`objc2-application-services` + `objc2-core-foundation`).
- `crates/openlogi-overlay/src/platform.rs` — the Actions Ring helper's window policy:
  accessory activation policy, non-activating borderless panel, the `NSEvent` global
  click-away monitor (`block2`), and `CGGetActiveDisplayList`/`CGDisplayBounds` for the
  cursor's display. It lives in its own crate because nothing else links it — but it is
  bound by every rule in this file.

Spawning the agent under its own macOS TCC identity (so its Accessibility /
Input-Monitoring grants aren't attributed to the GUI, issue #214) lives in the
external [`disclaim`](https://crates.io/crates/disclaim) crate — `posix_spawn` +
the private `responsibility_spawnattrs_setdisclaim`, not ObjC. `ipc_client`'s
`spawn_agent` uses it; there is no in-tree FFI for it.

`single_instance.rs` (fs4 lock), `launch_agent.rs` (plist via `std::fs`), `updater.rs`
(gpui_updater) contain **no** ObjC FFI — don't add any.

## Ownership: `Retained<T>`, never raw `id`

`objc2` makes ownership a value: a `Retained<T>` releases exactly once on `Drop`.
That is *why* this code can't reproduce issue #99 (a `+1` `NSString` leaked on every
2 s tray refresh under the old `cocoa`/`objc` 0.x path).

- Every string is `NSString::from_str(s)` → a `Retained<NSString>` used as a borrowed
  temporary; it releases at the end of the statement. **There is no `nsstring()` helper
  and no autorelease pool in the tray path** — don't reintroduce either.
- `alloc`/`init`/`new`/`copy` and the framework getters return `Retained<T>` /
  `Option<Retained<T>>`; you keep what you need in a field and let `Drop` free it.
- **Never** call manual `retain`/`release`/`autorelease`, add raw `cocoa`/`objc` 0.x, or
  build a bespoke retain/release helper layer — that re-derives `Retained<T>`, worse.

## Thread affinity is in the type system

- `NSMenu` and `NSMenuItem` are `#[thread_kind = MainThreadOnly]` → their `Retained` is
  `!Send`. `NSStatusItem`, `NSImage`, `NSWorkspace` are `AnyThread` (their `Retained` is
  still `!Send`, because a bare ObjC object is `!Sync`).
- Constructing a `MainThreadOnly` object needs a `MainThreadMarker` (`NSMenu::new(mtm)`,
  `NSMenuItem::alloc(mtm)`, `status_item.button(mtm)`). Mutating an already-held
  `Retained<NSMenuItem>` (`setTitle`/`setHidden`) does **not** — possessing the `!Send`
  handle already proves you're on the main thread.
- The tray's state lives in a **`thread_local`** (`TRAY`), not a `static`: a `Retained`
  of a `MainThreadOnly`/ObjC object can't satisfy a `Sync` static. `install`/`show_in_dock`/
  `hide_from_dock` obtain `mtm` via `MainThreadMarker::new()` at the GPUI→objc2 boundary
  (they always run on GPUI's main thread). Do **not** copy gpui's own
  `NSThread.isMainThread` + `dispatch2` runtime-check idiom here — we use the compile-time
  `MainThreadMarker` guarantee.

## Privacy permissions (TCC): typed framework crates, never a hand-rolled `extern`

There is no general TCC API: Apple ships no public way to enumerate or request TCC state
generically, and `TCC.db` is SIP-protected (reading it needs Full Disk Access). Crates that
paper over this exist — `permission-flow` covers many services — but none fit here, for the
reason in the rules below. Every permission is its own framework call, so "the TCC layer" is
just this table, and it lives in `openlogi-permissions`:

| Permission | Crate | Symbol |
|---|---|---|
| Accessibility | `objc2-application-services` (`HIServices` + `AXUIElement`) | `AXIsProcessTrusted` / `AXIsProcessTrustedWithOptions` |
| Input Monitoring / Post Event | `objc2-io-kit` (`hidsystem`) | `IOHIDCheckAccess` / `IOHIDRequestAccess` |
| Bluetooth | `objc2` class lookup (see below) | `+[CBManager authorization]` |
| Camera / microphone | `openlogi-camera` (`capture.rs`) | `+[AVCaptureDevice authorizationStatusForMediaType:]` |
| Screen Recording (unused) | `objc2-core-graphics` | `CGPreflightScreenCaptureAccess` |
| Full Disk Access (unused) | — | no API; only a probe of a protected path |

Rules:

- **Never re-declare these in a `#[link(name = "…", kind = "framework")] extern "C"` block**
  and never hardcode their discriminants. The generated bindings are typed
  (`IOHIDRequestType::ListenEvent`, `IOHIDAccessType::Granted`), which is the workspace rule
  about wire values in another guise — a bare `IOHIDCheckAccess(1) == 0` says nothing.
  `IOHIDCheckAccess` is a *safe* fn in `objc2-io-kit`; the AX pair is `unsafe` only because
  the options dictionary is untyped.
- Add these crates with `cargo add … --no-default-features --features <modules>` (they are
  huge and gated per C header), then declare the version once in the workspace table with
  `default-features = false` and pick features per crate. **Umbrella-feature trap:** a leaf
  feature is not enough — `AXUIElement` also needs `HIServices`, or the symbols silently
  don't exist.
- **Checking never prompts; prompting belongs to whoever owns the resource.** The agent
  raises the Accessibility prompt (it owns the tap) and opens HID; the GUI only reads status
  and deep-links to System Settings (`open_pane`). Don't call `IOHIDRequestAccess` or the
  `kAXTrustedCheckOptionPrompt` variant from the GUI — the grant would land on the wrong
  code-signing identity (issue #214, see `disclaim`). This split is also why the ready-made
  permission crates don't fit: they model one app asking for itself. `permission-flow`
  additionally brings its own onboarding UI and links the Swift runtime into every downstream
  binary; `macos-accessibility-client` is a raw-`extern` wrapper where this file requires
  typed bindings.
- `CBCentralManager.authorization` deliberately stays an `AnyClass::get` + `msg_send!` lookup
  rather than `objc2-core-bluetooth`: a missing class must degrade to `Unknown`, not panic.
- Deliberate raw-FFI exceptions, all of them symbols with no bindings to migrate to:
  `CGEventCopyIOHIDEvent` / `IOHIDEventGetSenderID` (undocumented, in the hook) and
  `responsibility_spawnattrs_setdisclaim` (private SPI, in the `disclaim` crate).
- Not migrated yet, don't copy the pattern: `openlogi-inject`'s `ax_nav` block (raw
  `AXUIElement` navigation + manual `CFRetain`/`CFRelease`) and `openlogi-camera`'s
  `AVAuthorizationStatus` integers — both have typed homes (`objc2-application-services`,
  `objc2-av-foundation`) whenever someone touches them next.

## The `unsafe` that remains (and the `# SAFETY` rule)

`objc2` marks only a few calls `unsafe`; each `unsafe` block does one operation with a
`SAFETY` comment (workspace lint policy). The current set:

- `NSMenuItem::initWithTitle_action_keyEquivalent` + `setTarget:` (raw selector; target is a
  *weak* reference, so the tray retains `MenuTarget` for the app's lifetime).
- `msg_send![super(this), init]` in `MenuTarget::new`.
- `NSString::to_str(pool)` in the hook (borrow tied to the pool).
- the hook's `AXIsProcessTrusted[WithOptions]` calls and the two extern statics they need
  (`kAXTrustedCheckOptionPrompt`, `kCFBooleanTrue`), plus the `CBCentralManager`
  class-method send. `IOHIDCheckAccess` needs none — `objc2-io-kit` exposes it as safe.

`status_item.rs`/`tray.rs` opt into `#[expect(unsafe_code)]` locally; `unsafe_code` stays
`deny` for the gui crate otherwise.

## CGEventTap stays on `core-graphics` — on purpose

The event tap in `openlogi-hook/macos.rs` is **not** migrated. `objc2-core-graphics` 0.3
*does* expose `CGEvent::tap_create`/`tap_enable` (it's not an availability gap), but the
tap's Accessibility-revoke **freeze-hazard** state machine (the 500 ms run-loop slice +
self-disable on its own thread) is load-bearing and must stay byte-for-byte. Only the
`NSWorkspace` frontmost-app read moved to `objc2`. Don't "modernize" the tap casually.

## Off-main autorelease pools

Tray code needs no pool (it runs on the main run loop, and `Retained` frees deterministically).
The hook's `frontmost_bundle_id` runs on a watcher thread with no run loop, so it keeps an
explicit `objc2::rc::autoreleasepool` — that's the *only* place in this crate and the hook a
pool belongs. (`openlogi-core`'s `post_media_key` follows the same pattern for media-key
`NSEvent`s on the dispatch threads.)

## Dependencies

`cocoa` / `objc` 0.x are gone from this crate's and the hook's direct deps (they remain in
`Cargo.lock` only transitively via gpui — expected). Use `cargo add` for objc2 framework
crates, then **verify the `zed`/`gpui-component` git pins in `Cargo.lock` didn't move** (the
gpui pin is held only by the lock; a resolve can bump it — restore with `cargo update -p gpui
--precise <commit>`).

Every objc2 framework crate is declared once in the workspace table (`objc2-app-kit`,
`objc2-foundation`, `objc2-core-foundation`, `objc2-application-services`, `objc2-io-kit`)
with `default-features = false`, and each member picks only the feature modules it uses. A
new one belongs in that table too, not inline in a member manifest — the unified version is
what keeps a resolve from dragging the gpui pin along.

## Build & verify

The gui crate needs the real Xcode toolchain for gpui's Metal shader compile:
`DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`, `SDKROOT=$(xcrun --show-sdk-path)`,
`xcbuild` stripped from `PATH`. Behavioural checks (tray icon shows, Open/Quit fire, device
rows update) need the running app. Confirm an FFI memory fix with `leaks` over a multi-minute
session: the `CFString`/`NSString` count must stay **flat** (the empirical inverse of #99).
