# Handoff — OpenLogi with MX Master 4 Action Ring (Windows)

**Read this first if you are an agent setting this up on a second Windows PC.**
Everything needed is here: what this fork is, how to build it, how to install it
so it stays installed, how to configure the Action Ring, and the specific traps
that cost the first machine several hours. Follow it top to bottom.

---

## 1. What this is

Upstream [OpenLogi](https://github.com/AprilNEA/OpenLogi) is a native, local-first
replacement for Logitech Options+ (Rust; no account, no telemetry, plain-TOML
config). It did **not** support the MX Master 4's **Action Ring** — the haptic
force-pad on the thumb rest, marked with eight dots.

This fork adds that, Windows-first:

| Capability | Detail |
|---|---|
| Pad capture | The pad speaks only HID++ and emits no native HID events. The agent arms it (`0x19c0` force threshold `0x15a3` + `0x1b04` analytics reporting) and decodes its press events. |
| On-screen ring | Tap the pad → a ring of eight circles appears at the cursor. Aim and tap again (or click) to fire. Centre ✕ / `Esc` / click-outside cancels. |
| Ring folders | A slot can hold a sub-ring; firing it swaps the ring in place, centre becomes ← to go back. One level deep. |
| Full editor | In-app page: the ring drawn large with labelled slots, a live action panel, folder drill-in. |
| Rich actions | `Run` (URL / file / program, `||` for args, `%VAR%` expansion), `PasteText` (typed as Unicode, no clipboard clobber), `CustomShortcut` (record by pressing the keys), `Named` (your own label for any action). |
| Side buttons | Back/Forward on the MX Master 4 emit **no** native HID either — this fork diverts and dispatches them, so browser back/forward finally work. |

Everything is also available on the gesture button and plain buttons (e.g. the
DPI toggle), not just the ring.

**Licensing:** upstream is dual MIT/Apache-2.0 — fine to use and modify. The
`design/` brand assets are **proprietary**; keep this mirror private and do not
redistribute those publicly.

**Upstream status:** this work is proposed upstream as
[PR #436](https://github.com/AprilNEA/OpenLogi/pull/436). Until/unless it merges,
this fork is the only build with the ring.

---

## 2. Prerequisites (target machine)

- **Windows 10/11**, MX Master 4 paired via a **Logi Bolt receiver** (this is the
  verified path; Bluetooth-direct is untested for the ring).
- **Rust** with the **GNU** toolchain. MSVC also works *if* Visual Studio Build
  Tools are installed; the source machine had no VS, so it used GNU:
  ```bash
  rustup toolchain install stable-x86_64-pc-windows-gnu
  ```
- **Git**.
- **`fxc.exe`** (DirectX shader compiler) — **only for release builds**. GPUI
  precompiles HLSL in release. If the Windows SDK is not installed, extract it
  from the `Microsoft.Windows.SDK.CPP` NuGet package (it lives at
  `c\bin\<ver>\x64\fxc.exe`) and set `GPUI_FXC_PATH` to it. Debug builds compile
  shaders at runtime and do not need it.

> **Toolchain trap.** The repo's `rust-toolchain.toml` says `channel = "stable"`,
> which resolves to the host default (often MSVC). On a machine without VS this
> fails with `linker link.exe not found` — and confusingly, plain `cargo build`
> may work while `cargo clippy` fails, because one is a direct binary and the
> other goes through the rustup shim. **Pin every cargo command explicitly:**
> ```bash
> rustup run stable-x86_64-pc-windows-gnu cargo <build|clippy|test> ...
> ```

---

## 3. Get the code

Private Forgejo (same LAN / Tailscale):

| Route | URL |
|---|---|
| LAN | `http://10.0.0.140:3000/ultradaoto/openlogi.git` |
| Tailscale | `http://forgejo-lxc.tailb56dd.ts.net:3000/ultradaoto/openlogi.git` |

```bash
git clone http://10.0.0.140:3000/ultradaoto/openlogi.git
cd openlogi
git checkout feat/haptic-sense-panel      # <-- the ring lives here, NOT on master
```

Authenticate with a **Forgejo personal access token** (Settings → Applications →
Generate Token, scope `read:repository` is enough to clone). Use the token as the
password:

```bash
git clone http://<username>:<token>@10.0.0.140:3000/ultradaoto/openlogi.git
```

`master` on this mirror tracks upstream; **`feat/haptic-sense-panel` is the branch
you want.**

---

## 4. Build

```bash
# from the repo root
$env:GPUI_FXC_PATH = 'C:\path\to\fxc.exe'      # release builds only
rustup run stable-x86_64-pc-windows-gnu cargo build --release -p openlogi-agent -p openlogi-gui
```

Two binaries land in `target\release\`:

- `openlogi-agent.exe` — background service: owns the input hook and **all**
  device I/O.
- `openlogi-gui.exe` — the app window; a pure IPC client. It **spawns the agent
  itself**, so you normally launch only the GUI.

Optional sanity gate (matches CI):

```bash
rustup run stable-x86_64-pc-windows-gnu cargo clippy --workspace --all-targets -- -D warnings
rustup run stable-x86_64-pc-windows-gnu cargo test --workspace
```

---

## 5. Install — and why it matters

> ### ⚠️ The single worst trap: two OpenLogis on one machine
> If an official OpenLogi is already installed (MSI / winget), you now have two
> builds. Launching the **old** one from the Start menu starts an old agent that
> cannot parse the ring bindings, **falls back to defaults, and saves those
> defaults over `config.toml` — wiping every binding.** On the source machine
> this silently destroyed a fully configured ring.
>
> This fork now guards against it (config `schema_version` 4, and a build that
> cannot read the config refuses to save over it), but an *older* binary predates
> that guard. **Have exactly one OpenLogi on the machine.**

**Do this:** if an official OpenLogi is installed, either uninstall it, or
overwrite its binaries so every launch path runs this build:

```powershell
$dst = "$env:LOCALAPPDATA\Programs\OpenLogi"          # default install location
Stop-Process -Name OpenLogi, openlogi-gui, openlogi-agent -Force -ErrorAction SilentlyContinue
Copy-Item .\target\release\openlogi-gui.exe   "$dst\OpenLogi.exe"      -Force
Copy-Item .\target\release\openlogi-agent.exe "$dst\openlogi-agent.exe" -Force
```

If OpenLogi was never installed, create that folder and copy both binaries in
(rename the GUI to `OpenLogi.exe`), then make a Start-menu shortcut to it.

**Autostart** (one entry, GUI only — it starts the agent):

```powershell
Set-ItemProperty -Path 'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Run' `
  -Name 'OpenLogi' -Value "`"$env:LOCALAPPDATA\Programs\OpenLogi\OpenLogi.exe`" --background"
```

`--background` starts it resident without opening a window. Do **not** add a
second Run entry for the agent — two agents means two input hooks.

**Also uninstall Logi Options+ if present.** It re-arms the same pad and fights
for the device. Its updater service resurrects the app, so disabling is not
enough — uninstall it:
`"C:\Program Files\LogiOptionsPlus\logioptionsplus_updater.exe" --uninstall --full --shadow`
(elevated). G HUB is a separate stack and can stay.

---

## 6. Configure the Action Ring

Config lives at **`%USERPROFILE%\.config\openlogi\config.toml`** (plain TOML;
edit it with the app closed, or use the in-app editor).

### 6a. Find the device key — do not copy the source machine's

Every device is keyed by its **physical** identity, e.g.
`receiver:97b76948a846c55a:slot:2`. That receiver UID is unique to the source
machine's Bolt receiver. **Yours will differ.**

Run the app once (it writes an identity block for every device it sees), then:

```powershell
Select-String -Path "$env:USERPROFILE\.config\openlogi\config.toml" -Pattern '^\[devices\.' |
  ForEach-Object { $_.Line }
```

Use the key whose `identity.display_name` is `MX Master 4`. Substitute it
everywhere `receiver:97b76948a846c55a:slot:2` appears below.

### 6b. The ring layout

A ready-to-adapt example is in **[`docs/action-ring-example.toml`](docs/action-ring-example.toml)** —
copy the `ActionRing` sections into your config under your own device key.

Slots are compass-named clockwise from the top: `North`, `NorthEast`, `East`,
`SouthEast`, `South`, `SouthWest`, `West`, `NorthWest`.

Shape of each action:

```toml
North = "PlayPause"                                   # built-in action
East  = { Run = "https://chatgpt.com/" }              # URL, file, or program
South = { Run = 'C:\Tools\app.exe||--flag value' }    # || separates arguments
West  = { PasteText = "text typed at the cursor" }

[devices."<YOUR-KEY>".bindings.ActionRing.NorthWest.CustomShortcut]
modifiers = 4        # bitmask: 1=Cmd 2=Shift 4=Ctrl 8=Alt 16=Win
key_code  = 49       # macOS kVK code (49 = Space) — see below
display   = "Ctrl+Space"

# A folder = a nested ring (one level deep)
[devices."<YOUR-KEY>".bindings.ActionRing.NorthEast.Folder.North]
PasteText = "first item in the sub-ring"
```

**Don't hand-write `CustomShortcut`.** Use the app: ring editor → slot →
**Keyboard Shortcut…** → **Press Keys**, then physically press the chord and
release. It records and saves in one motion, including chords another app has
claimed globally. (`key_code` is a macOS virtual-key code because that is the
cross-platform storage format; the recorder handles the translation.)

**Naming:** any action takes an optional label shown on the ring instead of the
raw payload (`{ Named = { name = "Dictation", action = { ... } } }`). The dialog's
**Name** field writes this.

### 6c. Using it

- **Tap the pad** → ring opens at the cursor.
- **Move toward a slot, tap again** (or click it) → fires.
- **Folder slot** → ring swaps to its contents; centre becomes ← .
- **Cancel** → centre ✕, `Esc`, or click outside.
- **Edit** → open the app → device → Buttons → click the ring hotspot or its
  "8 actions" card → full editor page.

---

## 7. Traps and troubleshooting

| Symptom | Cause / fix |
|---|---|
| **Ring bindings reverted to defaults on their own** | An older OpenLogi build ran and overwrote the config. See §5 — remove the second install. Restore from a backup (`config.toml.bak-*`). |
| **Pad stops responding entirely** | The pad's analytics reporting can wedge in device RAM after repeated arm/kill cycles (dev churn). **Power-cycle the mouse** (switch off, on). This is device-side; restarting software will not clear it. |
| **Ring doesn't open, but the app runs** | Check the agent armed it. Launch with `OPENLOGI_LOG='warn,openlogi_hid=info'` and look for `control capture active ... action_ring=true`. |
| **Every mouse click opens the ring** | Only CID `0x01a0` may be armed. CIDs `0x0050`/`0x0051` are **left/right click telemetry** — arming those makes every click a pad press. (Fixed here; noted in case anyone edits the arming code.) |
| **Mouse pointer jerky / skipping after hours** | Was caused by a caching bug: devices whose pairing register reports an all-zero `unit_id` were re-probed *every tick* — a full 46-feature HID++ walk every ~2 s over the mouse's own radio link. Fixed on this branch. If it recurs, power-cycle the mouse and report it. |
| **Huge log file** | Don't run with bare `OPENLOGI_LOG=info` long-term — tarpc logs every IPC call (~200 MB in days). Use `OPENLOGI_LOG='warn,openlogi_hid=info'`. |
| **Device takes minutes to appear** | First feature walk on the Windows BLE stack takes 1–3 s; the probe budget is 3 s. Should be fine now; if a device never appears, that budget is the place to look. |
| **`cargo clippy` fails with `link.exe not found`** | Toolchain trap — see §2, pin to `stable-x86_64-pc-windows-gnu`. |
| **Release build panics "Failed to find fxc.exe"** | Set `GPUI_FXC_PATH` — see §2. |

---

## 8. Architecture orientation (for the agent)

Three tiers, one install. The **GUI is a pure IPC client**; the **agent** owns the
input hook and all device I/O.

| Crate | Role in this feature |
|---|---|
| `openlogi-hid` | `gesture.rs` arms the pad and decodes its events; `inventory/` enumerates devices and caches probes. |
| `openlogi-core` | `binding.rs` — the `Action` catalog, `RingSlot`, `Binding::Ring`; `config.rs` — TOML schema (**v4**). |
| `openlogi-agent-core` | tarpc IPC contract (`ipc.rs`, **PROTOCOL_VERSION 14**) — append-only wire format. |
| `openlogi-gui` | `ring.rs` = the overlay window; `mouse_model/ring_editor.rs` = the editor page; `windows/ring_action_editor.rs` = the payload dialog; `chord_recorder.rs` = key recording. |
| `openlogi-inject` | Synthesizes actions (`SendInput`, `ShellExecuteW`). |

**Two rules that will bite you:**

1. **The IPC wire format is append-only.** bincode encodes variant *indices* and
   tarpc encodes *method order*. Never reorder or insert — only append — and bump
   `PROTOCOL_VERSION` plus the golden tests in
   `crates/openlogi-agent-core/tests/wire_format.rs`.
2. **Bump `SCHEMA_VERSION` whenever you write config an older build can't
   represent.** That gate is the only thing standing between a downgrade and a
   wiped config.

Also worth knowing: synthesized keyboard chords **must dwell** (~120 ms held).
Apps that detect a global hotkey by *polling* key state — common for chords like
`Ctrl+Space` — never see an instantaneous press. `openlogi-inject` holds chords
for this reason; don't "optimize" it away.

Repo conventions live in `AGENTS.md` (+ `.claude/rules/`): conventional commits,
no AI attribution, i18n strings added to **all 20 locales** at the same position.

---

## 9. Quick checklist

1. [ ] Rust GNU toolchain installed; `fxc.exe` available; `GPUI_FXC_PATH` set.
2. [ ] Cloned from Forgejo, **on branch `feat/haptic-sense-panel`**.
3. [ ] `cargo build --release -p openlogi-agent -p openlogi-gui` (pinned toolchain).
4. [ ] Logi Options+ uninstalled; only **one** OpenLogi on the machine.
5. [ ] Binaries copied to `%LOCALAPPDATA%\Programs\OpenLogi\`; single Run key with `--background`.
6. [ ] App launched once; **device key** read out of `config.toml`.
7. [ ] Ring slots configured (in-app editor, or adapt `docs/action-ring-example.toml`).
8. [ ] Verified: log shows `action_ring=true`; tapping the pad opens the ring.
