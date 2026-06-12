# `platform/` — macOS native FFI

This directory is OpenLogi's macOS-native surface. The Objective-C FFI here runs
on **`objc2`** (0.6 / framework crates 0.3): `Retained<T>` smart pointers, typed
AppKit objects, `define_class!` for subclasses. The whole workspace's ObjC-runtime
FFI is exactly these files — keep them in sync:

- `status_item.rs` — safe `objc2` wrappers over `NSStatusItem` / `NSMenu` / `NSMenuItem`.
- `tray.rs` — the OpenLogi menu-bar semantics + the `OpenLogiMenuTarget` (`define_class!`).
- `permissions.rs` — `CBCentralManager.authorization` (`objc2` class lookup) + `IOHIDCheckAccess` (C FFI).
- `crates/openlogi-hook/src/macos.rs` — CGEventTap (on `core-graphics`, see below) + the `NSWorkspace` frontmost-app read (`objc2`).

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

## The `unsafe` that remains (and the `# SAFETY` rule)

`objc2` marks only a few calls `unsafe`; each `unsafe` block does one operation with a
`SAFETY` comment (workspace lint policy). The current set:

- `NSMenuItem::initWithTitle_action_keyEquivalent` + `setTarget:` (raw selector; target is a
  *weak* reference, so the tray retains `MenuTarget` for the app's lifetime).
- `msg_send![super(this), init]` in `MenuTarget::new`.
- `NSString::to_str(pool)` in the hook (borrow tied to the pool).
- the hook's accessibility C FFI + the `CBCentralManager` class-method send.

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

## Build & verify

The gui crate needs the real Xcode toolchain for gpui's Metal shader compile:
`DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`, `SDKROOT=$(xcrun --show-sdk-path)`,
`xcbuild` stripped from `PATH`. Behavioural checks (tray icon shows, Open/Quit fire, device
rows update) need the running app. Confirm an FFI memory fix with `leaks` over a multi-minute
session: the `CFString`/`NSString` count must stay **flat** (the empirical inverse of #99).
