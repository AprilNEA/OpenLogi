---
paths:
  - "crates/openlogi-desktop/src/platform/**"
  - "crates/openlogi-overlay/src/platform.rs"
  - "crates/openlogi-permissions/**"
  - "crates/openlogi-camera/**"
  - "crates/openlogi-agent/src/tray.rs"
  - "crates/openlogi-agent/src/status_item.rs"
  - "crates/openlogi-agent-core/src/watchers/camera.rs"
  - "crates/openlogi-hook/src/macos.rs"
  - "crates/openlogi-inject/src/inject/macos.rs"
  - "crates/openlogi-hid/src/permissions.rs"
---

# macOS native FFI

OpenLogi's Objective-C FFI runs on **`objc2`** (0.6 / framework crates 0.3):
`Retained<T>` smart pointers, typed framework objects, `define_class!` for
subclasses. It is spread across crates rather than one directory — the GUI owns
almost none of it. The whole workspace's native macOS surface is exactly these
files; **keep this table in sync when you add or move one**:

| File | What it carries |
|---|---|
| `openlogi-agent/src/status_item.rs` | safe `objc2` wrappers over `NSStatusItem` / `NSMenu` / `NSMenuItem` |
| `openlogi-agent/src/tray.rs` | the menu-bar semantics, `MenuTarget` + `ResumeTarget` (`define_class!`), the Accessory `NSApplication` loop, `NSWorkspace` resume notifications |
| `openlogi-agent-core/src/watchers/camera.rs` | the CoreMediaIO "camera is running" property read |
| `openlogi-camera/src/capture.rs` | `AVCaptureSession` capture + the `define_class!` frame delegate, and the Camera TCC prompt |
| `openlogi-camera/src/macos.rs` | `AVCaptureDevice` enumeration (`class!` + `msg_send!`) |
| `openlogi-camera/src/uvc.rs`, `.../uvc/iokit.rs` | IOKit USB / UVC control transfers; every `unsafe` in the macOS UVC backend lives in `iokit.rs` |
| `openlogi-desktop/src/platform/login_item.rs` | `SMAppService` registration of the agent's launchd service (the login-item side of the agent lifecycle; the GUI must own it — the API resolves the plist against the calling app's bundle) |
| `openlogi-desktop/src/platform/os.rs` | `NSProcessInfo` OS version + the `NSAppearance` titlebar sync |
| `openlogi-hid/src/permissions.rs` | `IOHIDCheckAccess` / `IOHIDRequestAccess` (the prompting half of Input Monitoring) |
| `openlogi-hook/src/macos.rs` | the CGEventTap (on `core-graphics`, see below), the `NSWorkspace` frontmost-app read, the Accessibility-trust check/prompt, and the HID sender-id lookup |
| `openlogi-inject/src/inject/macos.rs` | CGEvent synthesis, media-key `NSEvent`s, raw `AXUIElement` navigation, and the `dlopen`'d private SPIs |
| `openlogi-overlay/src/platform.rs` | the Actions Ring helper's window policy: accessory activation, non-activating panel, the `NSEvent` global click-away monitor (`block2`), and `CGGetActiveDisplayList` / `CGDisplayBounds` |
| `openlogi-permissions/src/macos.rs` | non-prompting permission reads + System-Settings deep links; `+[CBManager authorization]` via an `AnyClass` lookup |

Every rule below binds all of them, whichever crate they live in.

Spawning the agent under its own macOS TCC identity (so its Accessibility /
Input-Monitoring grants aren't attributed to the GUI, issue #214) lives in the
external [`disclaim`](https://crates.io/crates/disclaim) crate — `posix_spawn` +
the private `responsibility_spawnattrs_setdisclaim`, not ObjC.
`openlogi-desktop/src/services/ipc.rs`'s `spawn_agent` uses it; there is no
in-tree FFI for it. Likewise, installed-application discovery and icon
rendering for per-app profiles live in the external
[`appcatalog`](https://crates.io/crates/appcatalog) crate (`NSWorkspace` +
`NSBitmapImageRep` there, not here); `openlogi-desktop/src/platform/app_icon.rs`
only wraps its PNG bytes into a `gpui::Image`.

The rest of `openlogi-desktop/src/platform/` (`updater.rs`, on `gpui_updater`)
carries **no** ObjC FFI — don't add any. Neither do `openlogi-core`'s
`single_instance.rs` (fs4 lock) or `openlogi-agent`'s `launch_agent.rs` (plist
via `std::fs`).

## Ownership: `Retained<T>`, never raw `id`

`objc2` makes ownership a value: a `Retained<T>` releases exactly once on `Drop`.
That is *why* this code can't reproduce issue #99 (a `+1` `NSString` leaked on
every 2 s tray refresh under the old `cocoa`/`objc` 0.x path).

- Every string is `NSString::from_str(s)` → a `Retained<NSString>` used as a
  borrowed temporary; it releases at the end of the statement. **There is no
  `nsstring()` helper and no autorelease pool in the tray path** — don't
  reintroduce either.
- `alloc`/`init`/`new`/`copy` and the framework getters return `Retained<T>` /
  `Option<Retained<T>>`; you keep what you need and let `Drop` free it.
- IOKit handles get the same treatment by hand in `camera/src/uvc/iokit.rs`:
  `IoObject`, `UsbInterface` and `SeizedDevice` release on drop, and
  CoreFoundation values arrive as `CFRetained`. Never hand-balance a release.
- **Never** call manual `retain`/`release`/`autorelease`, add raw `cocoa`/`objc`
  0.x, or build a bespoke retain/release helper layer — that re-derives
  `Retained<T>`, worse. The one exception is the raw AX navigation in
  `openlogi-inject` (see below), which is on the migrate-when-touched list.

## Thread affinity is in the type system

- `NSMenu` and `NSMenuItem` are `#[thread_kind = MainThreadOnly]` → their
  `Retained` is `!Send`. `NSStatusItem`, `NSImage`, `NSWorkspace` are `AnyThread`
  (their `Retained` is still `!Send`, because a bare ObjC object is `!Sync`).
- Constructing a `MainThreadOnly` object needs a `MainThreadMarker`
  (`NSMenu::new(mtm)`, `NSMenuItem::alloc(mtm)`, `status_item.button(mtm)`).
  Mutating an already-held `Retained<NSMenuItem>` (`setTitle`/`setHidden`) does
  **not** — possessing the `!Send` handle already proves you're on the main
  thread.
- The `mtm` is obtained with `MainThreadMarker::new()` at each process's
  entry into AppKit — `tray::run_app_loop` in the agent, `configure_application`
  in the overlay, `set_app_appearance` in the GUI — and threaded down from
  there. Do **not** copy gpui's own `NSThread.isMainThread` + `dispatch2`
  runtime-check idiom; we use the compile-time `MainThreadMarker` guarantee.
- The tray needs no `static` and no `thread_local`: `run_app_loop` is `-> !`, so
  the status item, its `MenuTarget` and the `ResumeTarget` are bound as locals
  that outlive `NSApplication::run()`. They must stay bound — menu items
  reference their target *weakly*, and the notification center does the same.
- `openlogi-camera`'s frame delegate is deliberately the opposite: an ivar-less
  `NSObject` subclass with no `thread_kind`, because AVFoundation drives it on a
  background dispatch queue.

## Privacy permissions (TCC): typed framework crates, never a hand-rolled `extern`

There is no general TCC API: Apple ships no public way to enumerate or request
TCC state generically, and `TCC.db` is SIP-protected (reading it needs Full Disk
Access). Crates that paper over this exist — `permission-flow` covers many
services — but none fit here, for the reason in the rules below. Every permission
is its own framework call, so "the TCC layer" is just this table:

| Permission | Crate | Symbol | Read / prompt |
|---|---|---|---|
| Accessibility | `objc2-application-services` (`HIServices` + `AXUIElement`) | `AXIsProcessTrusted` / `AXIsProcessTrustedWithOptions` | both in `openlogi-hook` |
| Input Monitoring / Post Event | `objc2-io-kit` (`hidsystem`) | `IOHIDCheckAccess` / `IOHIDRequestAccess` | read in `openlogi-permissions`, prompt in `openlogi-hid` |
| Bluetooth | `objc2` class lookup (see below) | `+[CBManager authorization]` | `openlogi-permissions` |
| Camera / microphone | `openlogi-camera` (`capture.rs`) | `+[AVCaptureDevice authorizationStatusForMediaType:]` / `requestAccessForMediaType:` | `openlogi-camera` |
| Screen Recording (unused) | `objc2-core-graphics` | `CGPreflightScreenCaptureAccess` | — |
| Full Disk Access (unused) | — | no API; only a probe of a protected path | — |

Rules:

- **Never re-declare a permission API in a `#[link(name = "…", kind =
  "framework")] extern "C"` block** and never hardcode its discriminants. The
  generated bindings are typed (`IOHIDRequestType::ListenEvent`,
  `IOHIDAccessType::Granted`), which is the workspace rule about wire values in
  another guise — a bare `IOHIDCheckAccess(1) == 0` says nothing.
  `IOHIDCheckAccess` is a *safe* fn in `objc2-io-kit`; the AX pair is `unsafe`
  only because the options dictionary is untyped.
- Add these crates with `cargo add … --no-default-features --features <modules>`
  (they are huge and gated per C header), then declare the version once in the
  workspace table with `default-features = false` and pick features per crate.
  **Umbrella-feature trap:** a leaf feature is not enough — `AXUIElement` also
  needs `HIServices`, or the symbols silently don't exist.
- **Checking never prompts; prompting belongs to whoever owns the resource.**
  The agent raises the Accessibility prompt (it owns the tap) and calls
  `IOHIDRequestAccess` before opening HID; the GUI only reads status and
  deep-links to System Settings (`open_pane`). Never call the prompting half from
  the GUI — the grant would land on the wrong code-signing identity (issue #214,
  see `disclaim`). This split is also why the ready-made permission crates don't
  fit: they model one app asking for itself. `permission-flow` additionally
  brings its own onboarding UI and links the Swift runtime into every downstream
  binary; `macos-accessibility-client` is a raw-`extern` wrapper where this file
  requires typed bindings.
- **TCC matches the full Designated Requirement, not the bundle ID** ([TN3127]).
  Re-signing one bundle ID with a different *kind* of certificate leaves a record
  that can never match again, and toggling the checkbox does not rewrite the
  stored `csreq`. Not hypothetical: 0.6.24–0.6.26 shipped `.dev` bundle IDs
  signed with Developer ID (`a344e22f`), so a dev build of the same ID signed
  *Apple Development* meets a stale Developer ID requirement and `tccd` logs
  `Failed to match existing code requirement`. Suspect this first when a grant
  silently does nothing; `tccutil reset <service> <bundle-id>` clears the record,
  and a never-used bundle ID (or a fresh user/VM) is the only clean re-test.
- **Accessibility does not use `tccd`'s generic consent sheet**, so its logs read
  oddly: a request answers *"Service kTCCServiceAccessibility does not allow
  prompting; returning Unknown"* with `DB Action:None`. That describes the `tccd`
  preflight **only** — the flow continues into `universalAccessAuthWarn`, which
  makes its own `TCCAccessCopyInformation` call and pre-creates an unchecked row
  when no matching record exists. Reading `DB Action:None` as "nothing is ever
  written" is wrong, and it points debugging away from the stale-requirement case
  above.
- `CBCentralManager.authorization` deliberately stays an `AnyClass::get` +
  `msg_send!` lookup rather than `objc2-core-bluetooth`: a missing class must
  degrade to `Unknown`, not panic.

[TN3127]: https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements

## Raw `extern` blocks: only where no bindings exist

Typed framework crates are the default; a hand-written `unsafe extern "C"` block
is allowed **only** for symbols objc2 has no bindings for, and it stays next to
its single user. The current set, all deliberate:

- `openlogi-hook`: `CGEventCopyIOHIDEvent` / `IOHIDEventGetSenderID` —
  undocumented, no bindings anywhere.
- `openlogi-camera`: the AVFoundation / CoreMedia / CoreVideo / CoreFoundation
  statics and functions its capture and enumeration paths need
  (`AVMediaTypeVideo`, `CMSampleBufferGetImageBuffer`, the `CVPixelBuffer`
  accessors, `CFRunLoopRunInMode`, `dispatch_queue_create`) — AVFoundation has no
  typed framework crate in the tree.
- `openlogi-agent-core/src/watchers/camera.rs`: the CoreMediaIO property API —
  same reason.
- `openlogi-inject`: the `AXUIElement` subset it navigates with, plus
  `CFRetain`/`CFRelease`, and the `dlopen`/`dlsym`-resolved private SPIs
  (`CoreDockSendNotification`, the CGS symbolic-hotkey trio).
- the `disclaim` crate: `responsibility_spawnattrs_setdisclaim` (private SPI).

Two of those are on the migrate-when-touched list rather than permanent:
`openlogi-inject`'s raw AX navigation with its manual `CFRetain`/`CFRelease`
belongs in `objc2-application-services`, and `openlogi-camera`'s
`AVAuthorizationStatus` integers belong in `objc2-av-foundation`. Don't copy
either pattern into new code.

## The `unsafe` that remains (and the `SAFETY` rule)

`unsafe_code` is `deny` workspace-wide; every file that needs it opts in with a
scoped `#[expect(unsafe_code, reason = "…")]`, and every block does one operation
under a `SAFETY` comment. Where it currently lives on macOS:

- `agent/status_item.rs` — `NSMenuItem::initWithTitle_action_keyEquivalent` +
  `setTarget:` (raw selector; the target is a *weak* reference, which is why the
  tray keeps `MenuTarget` alive for the app's lifetime).
- `agent/tray.rs` — `msg_send![super(this), init]`, the notification-center
  `addObserver:selector:name:object:`, and the `NSWorkspace*Notification` name
  statics.
- `hook/macos.rs` — the whole tap (Core Graphics / Core Foundation C APIs),
  `AXIsProcessTrusted[WithOptions]` and the two extern statics they need
  (`kAXTrustedCheckOptionPrompt`, `kCFBooleanTrue`), and `NSString::to_str(pool)`
  (the borrow is tied to the pool).
- `permissions/macos.rs` — the CoreBluetooth force-link and the `CBManager`
  class-method send. `IOHIDCheckAccess` needs none: `objc2-io-kit` exposes it as
  a safe fn, in `openlogi-permissions` and `openlogi-hid` alike.
- `overlay/platform.rs` — `NSEvent::removeMonitor` and the
  `CGGetActiveDisplayList` / `CGDisplayBounds` pair.
- `desktop/platform/os.rs` — reading AppKit's `NSAppearanceName` statics to set
  `NSApp.appearance`.
- `desktop/platform/login_item.rs` — the `SMAppService` calls (all generated
  bindings are `unsafe fn`s) and the `SMAppServiceErrorDomain` extern static.
  Together with `os.rs`, the GUI's entire `unsafe` surface.
- `camera/{capture,uvc/iokit}.rs` — the AVFoundation capture FFI and the IOKit
  USB plug-in; `uvc/iokit.rs` deliberately concentrates every `unsafe` of the
  macOS UVC backend so the descriptor parser above it is ordinary safe code.

## CGEventTap stays on `core-graphics` — on purpose

The event tap in `openlogi-hook/src/macos.rs` is **not** migrated.
`objc2-core-graphics` 0.3 *does* expose `CGEvent::tap_create`/`tap_enable` (it's
not an availability gap), but the tap's Accessibility-revoke **freeze-hazard**
state machine (the 500 ms run-loop slice + self-disable on its own thread) is
load-bearing and must stay byte-for-byte. Only the `NSWorkspace` frontmost-app
read moved to `objc2`. Don't "modernize" the tap casually.

## Off-main autorelease pools

Code on the main run loop needs no pool (`Retained` frees deterministically);
code on a bare thread does, because the framework still autoreleases internal
temporaries. The three places that keep an explicit `objc2::rc::autoreleasepool`,
and the only ones that should:

- `openlogi-hook`'s `frontmost_application` — a watcher thread with no run loop,
  and `to_str` borrows its UTF-8 view from the pool (both the bundle id and the
  localized name).
- `openlogi-inject`'s `post_media_key` — the hook/gesture dispatch threads, where
  both the `NSEvent` creation and the `CGEvent` getter autorelease temporaries.
- `openlogi-camera`'s device enumeration — every `AVCaptureDevice` string is
  copied out before the pool drains, so no `Retained<T>` escapes it.

## Dependencies

`cocoa` / `objc` 0.x are gone from every crate's direct deps (they remain in
`Cargo.lock` only transitively via gpui — expected). Use `cargo add` for objc2
framework crates, then **verify the `zed` / `gpui-component` git pins in
`Cargo.lock` didn't move** (the gpui pin is held only by the lock; a resolve can
bump it — restore with `cargo update -p gpui --precise <commit>`).

Every ObjC / Core-framework crate is declared **once** in the workspace table —
`objc2`, `objc2-app-kit`, `objc2-foundation`, `objc2-core-foundation`,
`objc2-core-graphics`, `objc2-application-services`, `objc2-io-kit`,
`objc2-service-management`, `block2`, `core-graphics`, `core-foundation`. The header-gated ones carry
`default-features = false` there, and each member inherits with
`workspace = true` and adds only the feature modules it uses. A new one belongs
in that table too, never inline in a member manifest: the unified version is what
keeps a resolve from dragging the gpui pin along. Trim a member's feature list
when the code that needed it moves out.

## Build & verify

The GUI crates need the real Xcode toolchain for gpui's Metal shader compile:
`DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`,
`SDKROOT=$(xcrun --show-sdk-path)`, `xcbuild` stripped from `PATH`. Behavioural
checks (tray icon shows, Open/Quit fire, device rows update, the ring follows the
cursor's display) need the running app. Confirm an FFI memory fix with `leaks`
over a multi-minute session: the `CFString`/`NSString` count must stay **flat**
(the empirical inverse of #99).
