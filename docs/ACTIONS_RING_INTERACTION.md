# Actions Ring: interaction and motion plan

Status: phases 1–2 are implemented in `crates/openlogi-overlay/src/ring.rs`;
phases 3–4 remain proposals. Haptics are already implemented and are listed
here only for completeness.

The reference prototype lives at
[`docs/assets/actions-ring/prototype.html`](assets/actions-ring/prototype.html)
(open it in a browser; `?state=ai` shows the expanded submenu).

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

- Per-slot tooltip on a 100 ms dwell, flipped below when the slot is near the
  top of the panel and clamped to its horizontal bounds.
- Hover inverts the slot to a near-black fill; the accent is no longer spent on
  a transient hover.
- Entrance motion: slots travel from the hub to their positions together and
  the panel fades in. GPUI's `with_animation` respects the host's reduced-motion
  setting, so it renders one static frame when that is on.

The close animation is not implemented: the overlay hides the window as soon as
the agent dismisses the session, so animating the exit would mean delaying
teardown. That belongs with phase 3, where the submenu needs a teardown story of
its own.

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

### 3. AI submenu on hover

A slot whose presentation carries submenu entries expands into a provider arc.

**Trigger and dismissal**

| Event | Result |
| --- | --- |
| Hover the slot for 120 ms | Expand |
| Hover a provider | Update the tooltip (`{verb} with {provider}`) |
| Pointer outside the ring and provider arc for 400 ms | Collapse to the ring |
| Hover the hub | Collapse |
| Click the hub | Dismiss the whole session |
| `Escape` | Collapse, then dismiss on a second press |

**The ring must not move.** Expansion adds space to the right of the panel; it
never translates, scales, or re-lays out the eight slots or the hub.

**Window constraint.** 360×360 has no room for a provider arc. Three options:

1. **Grow the graphics to 520×360 while expanded, keeping the ring's screen
   centre fixed** (preferred). The ring is drawn at `(180, 180)` in both
   states; only the canvas to the right is added. The window origin does not
   change, so the ring does not shift.
2. A separate always-on-top window for the arc. More moving parts, and two
   windows to keep in step for click-away.
3. Place providers inside the panel. Rejected — they would overlap the ring.

When the ring is clamped near the right edge of a display, the arc should flip
to the left instead of growing off-screen. The flip is a placement decision, not
a second layout.

**Pointer and click-away.** `RingPointer` and `ClickAwaySession` currently treat
the 360×360 panel as the ring. They must treat the expanded bounds as the ring
for the duration of the submenu, or moving to a provider would count as a
click-away.

**IPC.** `ActionRingPresentation` gains
`submenu: Vec<ActionRingSubmenuEntry>` (empty when the slot has none), where an
entry carries a stable index, label, icon and the verb used in the tooltip. The
overlay reports `Activate { slot, submenu: Some(index) }`; the agent resolves
the entry against its session and executes it. Wire types are append-only:
adding a field needs `PROTOCOL_VERSION` above the latest release and regenerated
`crates/openlogi-ipc/tests/wire_format.rs` goldens.

### 4. Open and close motion

- **Open:** all eight slots travel from the hub to their positions together —
  no stagger — with `scale 0.32 → 1`, over **340 ms** ease-out. The panel fades
  in with them.
- **Close:** the reverse, **260 ms**, ending at the hub with opacity 0.
- Providers use the same motion on expand/collapse, all at once.
- GPUI's `with_animation` (already used in
  `crates/openlogi-desktop/src/features/keyboard/function_row.rs`) is the
  intended primitive.
- The window must still hide on dismissal; animation must not delay
  `dismiss_from_window`, or a cancelled session would leave an invisible window
  swallowing clicks.

### 5. Notification dot

A small dot at the hovered slot's top-right (the AI slot in the reference),
hidden while the submenu is open. It carries no new interaction: the slot
activates as before.

- Presentation needs one bit: `badge: bool` on `ActionRingPresentation`.
- Render as a 12 px accent dot with a 2.5 px panel-coloured ring so it separates
  from the slot without a second outline.

## Phases

| Phase | Change | Wire impact | Status |
| --- | --- | --- | --- |
| 1 | Tooltip, hover inversion | None — overlay only | Implemented |
| 2 | Open motion (close deferred) | None — overlay only | Implemented |
| 3 | AI submenu, window growth, provider arc | `submenu` field, agent resolution | Proposed |
| 4 | Notification dot | `badge` field | Proposed |

Phases 3–4 need a product decision before the wire changes: what an "AI action"
is, and which actions carry a submenu.

## Acceptance

- [ ] Hovering a slot shows its label after 100 ms; leaving before that shows
      nothing.
- [ ] The tooltip never leaves the panel and its tail stays over the slot.
- [ ] Hovering inverts the slot; the accent appears only in the expanded state.
- [ ] Entering the submenu changes no slot or hub coordinate.
- [ ] Collapsing restores the ring with no relayout.
- [ ] A submenu near a screen edge opens on the side with room.
- [ ] Motion runs once per open, does not stagger, and never delays dismissal.
- [ ] Hover and activation still play `SubtleCollision` and `DampStateChange`;
      provider hover reuses the hover waveform rather than adding a new one.
- [ ] `cargo test -p openlogi-ipc --test wire_format` passes after phases 3–4.

## Risks and open questions

- **Click-away bounds.** The expanded window is larger than the panel; if
  `ClickAwaySession` keeps using panel bounds, providers become "outside" and
  the session dismisses on hover.
- **Wayland.** There the window already covers the display, so growing it is a
  no-op; the arc placement logic still has to clamp.
- **Reduced motion.** GPUI exposes no cross-platform reduced-motion flag in this
  workspace today. If one is added, phases 1–2 should honour it.
- **Submenu ownership.** The overlay holds the expanded state because it is
  presentation; the agent remains the only owner of executable actions. If a
  submenu should survive a snapshot refresh, the expanded slot has to move into
  the agent's session. That is a deliberate design choice, not an oversight.
- **Slot semantics.** Is "has a submenu" a property of the action or of the
  slot? The reference treats it as a property of the AI action, independent of
  where it is placed.
