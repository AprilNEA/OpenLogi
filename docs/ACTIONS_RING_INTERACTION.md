# Actions Ring: interaction and motion plan

Status: tooltip, hover treatment, system appearance, and reduced-motion handling
are implemented in `crates/openlogi-overlay/src/ring.rs` and
`crates/openlogi-overlay/src/platform/motion.rs`. Haptics are already
implemented and are listed here only for completeness.

## What existed before this change

| Area | Current behaviour |
| --- | --- |
| Panel | 360×360, centred on the cursor and clamped to its display (`WINDOW_SIZE`) |
| Slots | Eight fixed positions (`ActionRingSlot`), 54 px circles at radius 122 |
| Cancel | 48 px hub; click cancels the session |
| Hover | Slot filled with the accent and gained a 2 px accent border |
| Label | One label, fixed below the panel — not attached to the hovered slot |
| Motion | None; the panel appeared and disappeared at full opacity |
| Haptics | Hover plays `SubtleCollision` (waveform 4); activation plays `DampStateChange` (waveform 1) — `crates/openlogi-agent/src/server.rs` |

Presentation crosses the IPC boundary as
`ActionRingInvocation { session_id, slots: BTreeMap<ActionRingSlot, ActionRingPresentation>, language }`
(`crates/openlogi-ipc/src/ipc.rs`). Executable actions stay in the agent; the
overlay only reports `Hover`, `Activate` and `Cancel`.

## Implemented in this change

- Per-slot labels on a 100 ms dwell, with speech-bubble placement that flips
  when a slot is near the top of the ring.
- Hover selects a slot with accent fill and glow; idle slots stay translucent
  and follow the system light/dark appearance.
- Before each ring opens the overlay reads the OS reduced-motion preference off
  the UI thread (macOS Reduce Motion, Windows client-area animation setting,
  Linux GNOME `enable-animations` via `gsettings` bounded to 200 ms), applies
  it to GPUI's `reduce_motion`, and uses the last known value when a read fails
  or times out (motion on when none is known yet). While that read runs the
  overlay keeps reading requests, a dismissal cancels the pending open at once,
  and a newer request replaces it; the previous ring is already closed.

## Proposed changes

### 1. Per-slot tooltip

Replace the single label below the panel with a pill anchored to the hovered
slot.

- Appears after a **100 ms** dwell, so sweeping across the ring does not flash
  labels. Keyboard activation, if added later, should show it immediately.
- Prefers above the slot with a 12 px gap; flips below when the slot is too
  close to the top of the ring.
- Clamped to the panel's horizontal bounds; the tail tracks the slot centre when
  the pill is pushed sideways.
- The existing bottom label is removed to avoid showing the same text twice.

### 2. Hover treatment

Invert the slot instead of tinting it: near-black fill and the light glyph,
keeping the accent for the expanded (submenu) state only.

- Resting `SLOT_RESTING` (0.16 lightness) → hovered `neutral(0.10)`.
- The accent stays meaningful — it is not spent on a transient hover.
- Providers in a submenu use the same inversion.

## Phases

| Phase | Change | Wire impact | Status |
| --- | --- | --- | --- |
| 1 | Tooltip, hover inversion | None — overlay only | Implemented |
| 2 | Reduced motion at open | None — overlay only | Implemented |

## Acceptance

- [ ] Hovering a slot shows its label after 100 ms; leaving before that shows
      nothing.
- [ ] The tooltip never leaves the panel and its tail stays over the slot.
- [ ] Hovering inverts the slot; the accent appears only in the expanded state.
- [ ] Motion runs once per open, does not stagger, and never delays dismissal.
- [ ] Hover and activation still play `SubtleCollision` and `DampStateChange`.

## Risks and open questions

- **Wayland.** Multi-display placement and click-away still depend on one window
  per display; label and beam geometry must stay consistent when the pointer
  moves between outputs.
- **Reduced motion.** Before each ring opens the overlay reads the OS
  preference off the UI thread (macOS Reduce Motion, Windows client-area
  animation, Linux GNOME `enable-animations` via `gsettings` bounded to 200 ms)
  and applies it to GPUI `reduce_motion` before the ring is shown. A failed or
  timed-out read uses the last known value, or motion on when none is known yet.
  While the read runs the overlay keeps reading requests, a dismissal cancels
  the pending open at once, and a newer request replaces it; the previous ring
  is already closed.
