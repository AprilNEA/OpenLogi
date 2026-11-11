# MX Creative Console — reverse-engineering ledger

Evidence for the MX Creative Keypad and MX Creative Dialpad support. Every
wire fact OpenLogi relies on is listed here with its source. A fact without
hardware or shipped-artifact evidence stays in **Open questions**; it is not
implemented.

Evidence kinds:

- **HW**: observed on real hardware with the probes in
  `crates/openlogi-hid/examples/` (`mx_dialpad_probe`, `mx_keypad_*_probe`).
- **OPT+**: read statically from the shipped Logi Options+ / Logi Plugin
  Service artifacts (strings, protobuf descriptors, .NET metadata). No
  Logitech code is copied; only names and layouts are used as hints.

## Identity

| Device | VID:PID | Transport | Firmware | Evidence |
|---|---|---|---|---|
| MX Creative Keypad | `046d:c354` | USB-C, HID++ 4.5, direct (`0xff43:0x1a02`) | `U1 66.00_B0017` | HW 2026-11-03, Windows 11 (`openlogi diag features`) |
| MX Creative Dialpad | `046d:bc00` | Bluetooth LE, direct (`0xff43:0x0202`) | `RBO07.00_B0016` | HW 2026-11-03, Windows 11 (`openlogi list`, `mx_dialpad_probe nodes`) |

The dialpad answers `0x0005` with the name "MX Creative Dialpad" and reports
battery through `0x1004`.

### Dialpad feature table (HW)

`0000 0001 0003 0005 1d4b 0020 0021 0007 0011 1004 1b04(v6, idx 10) 1814 1815
4610(v1, idx 13) 00c3` plus hidden/engineering features
`1802 1803 1807 1816 1805 1830 1891 18a1 1e00 1e02 1602 1eb0 1861 18b1 9205`.

There is no `0x4600` Crown and no `0x2150` Thumbwheel. The rotary controls
are served by `0x4610` alone.

### Keypad feature table (HW, Windows; matches the macOS fixture)

`0000 0001 0003 0005 0008 0011 00c3 1602 1802 1805 1807 1b04(v6, idx 11)
1e00 1e02 1eb0 8040(v1, idx 15) 92e2 180b`.

## Dialpad: feature `0x4610` MultiRoller

Name source: OPT+. `logioptionsplus_agent.exe` ships
`devio\features\feature_4610_multi_roller.cpp` and the protobuf descriptor
`logi/protocol/multiroller.proto`. That descriptor gives the field names below
(`MultiRollerCapabilities`, `RollerCapabilities`, `RollerMode{NATIVE,DIVERTED}`,
`RollerSettings{direction NORMAL|INVERTED, current_speed}` and the
`MultiRollerRotationEvent{roller_id, delta, timestamp}`). The byte layout is
from HW only.

| Fn | Name (OPT+) | Request | Response (HW) |
|---|---|---|---|
| 0 | getCapabilities | — | `[0]` roller count = `2`; rest `0` |
| 1 | getRollerCapabilities | `[0]` roller id | dial (id 1) `b4 00 1f`, roller (id 0) `28 00 1f` |
| 2 | getMode | `[0]` roller id | `[0]` mode; `01` while Logi Options+ was diverting |
| 3 | setMode | `[0]` roller id, `[1]` mode | echoes `[id, mode]`; unknown id → HID++ error `0x02` (invalid argument) |
| ≥4 | — | — | HID++ error `0x07` (invalid function id) |

Notes:

- Roller ids: **1 = the large dial**, **0 = the small roller** (HW: the dial was
  turned first and reported id 1, then the roller reported id 0).
- getRollerCapabilities byte 0 is the increment count per revolution:
  `0xb4` = 180 for the dial, `0x28` = 40 for the roller. Bytes 1–2
  (`00 1f`) are taken to be the min/max speed of `RollerCapabilities`
  (OPT+ field names). That is an inference; OpenLogi does not depend on it.
- Mode `1` = diverted (HW: events arrived while `getMode` answered `01`). Mode
  `0` = native, per the `NATIVE = 0` enum order (OPT+); native-mode output
  is not yet observed.
- Direction and speed are host-side settings. The device has no
  get/setRollerSettings function (fn 4 returns `0x07`). Options+ keeps them in
  `RollerSettings` and the Loupedeck layer in
  `Loupedeck7DeviceSettingName::{EncoderReverseClicks, EncoderClicksMultiplier}`.

### Rotation event (HW)

```
11 ff 0d 00 RR DD TT TT TT TT 00 …
```

- `RR`: roller id (`01` dial, `00` roller).
- `DD`: signed `i8` delta in increments. Positive = clockwise / up. Observed
  values: `±1`, `-2`, `+2`, `+3`, `+4` on fast turns.
- `TT…`: big-endian `u32` device timestamp in milliseconds (wall-clock deltas
  matched within ±5 %).

Excerpt (dial, then roller):

```
10.73s  11 ff 0d 00 01 ff 00 32 7e 97
11.70s  11 ff 0d 00 01 01 00 32 82 99
34.50s  11 ff 0d 00 00 04 00 32 da a3
35.68s  11 ff 0d 00 00 ff 00 32 df 97
```

## Dialpad: buttons (`0x1b04` v6)

| CID | Task id | Flags | Capabilities (HW) |
|---|---|---|---|
| `0x0053` | `0x003c` | `0x0431` | divertable, analytics-events |
| `0x0056` | `0x003e` | `0x0431` | divertable, analytics-events |
| `0x0059` | `0x0109` | `0x0431` | divertable, analytics-events |
| `0x005a` | `0x0109` | `0x0431` | divertable, analytics-events |

Diverted press and release use the standard `divertedButtonsEvent` (HW): for
example `11 ff 0a 00 00 53 …` on press and `11 ff 0a 00 00 00 …` on release.
Logi Options+ keeps all four CIDs diverted while it runs.

Physical positions (HW, user-reported press order matched against the
capture): `0x0053` top left, `0x0056` top right, `0x0059` bottom left,
`0x005a` bottom right.

## Keypad on Windows: vendor channel collections (HW)

Windows splits the keypad's interface into one handle per top-level
collection. The vendor channel that shares the HID++ node on macOS lands on
two separate collections:

| Collection | Usage | Declared lengths (in / out) | Carries |
|---|---|---|---|
| Col01 | `ff43:1a02` | HID++ | HID++ long `0x11` |
| Col02 | `ff43:1a08` | 32 / 32 | input `0x13`: grid-key frames and image acks (`13 ff 02 2b c1 00 01`) |
| Col03 | `ff43:1a10` | 0 / 4095 | output `0x14`: key images; no input (a read fails with `ERROR_INVALID_USER_BUFFER`) |

On Col01, writing `0x14` fails with `ERROR_INVALID_FUNCTION`. The Windows
transport therefore opens the `0xff43` siblings of the HID++ node, reads them
alongside HID++, and routes each vendor report to the collection that declares
it (#2283). Verified 2026-11-03 against the same three collections: images
written to Col03 painted all nine keys and were acked on Col02, and the agent
painted bound key faces and dispatched grid presses.

## Keypad brightness: `0x8040` BrightnessControl (HW)

- `getInfo` (fn 0) → `00 64 65 10 00 00`: max `100`, min `0`, `101` steps,
  capabilities `0x10`.
- `getBrightness` (fn 1) → `00 46`, i.e. `70`, as Logi Options+ left it.
- `getIllumination` (fn 3) → HID++ error `0x05`. The panel has no on/off
  switch.
- OpenLogi writes `key_display_brightness` (1–100 %) scaled onto the
  reported range. It never writes the bottom value. Writing `30` read back
  as `00 1e`.

Brightness was stepped 100 → 5 → 60 % through
`openlogi api --save key-brightness`. The readbacks were `0x64`, `0x05` and
`0x3c`, and the user saw each change on the panel.

## Keypad LCD faces (HW)

Faces are rendered on the host and sent as one baseline JPEG per key; the
keypad only places the image. Verified on Windows 11 (2026-11-03) with:

- icon + label on a coloured background,
- picture + icon + label (a band behind the label keeps it legible),
- label only, wrapped over two lines,
- picture only.

A picture tile encoded at quality 90 needed two image packets, and the write
timed out waiting for its ack. Single-packet writes are the only verified
path, so the renderer lowers the JPEG quality step by step until the tile fits
one packet (`KEY_IMAGE_SINGLE_PACKET_MAX` = 4075 bytes). The multi-packet ack
behaviour is listed under **Open questions**.

## Dialpad `0x0005` DeviceType (HW)

`getDeviceType` answers `0x18`, a type the published `0x0005` table does not
list. The kind therefore comes from the registry (`DeviceKind::Dialpad`),
the same way the keypad's does.

## End-to-end verification (HW, Windows 11, 2026-11-03)

The dev agent ran with Logi Options+ stopped. Results:

- Dialpad: dial → volume up/down, roller → vertical scroll, top-left key →
  play/pause.
- Keypad: bound grid keys showed their icons and fired their actions.

## Logi Options+ architecture (OPT+)

- The Logi Plugin Service (`LoupedeckService.dll`, .NET 10, SHA-256
  `1e924b5a…f835`) models the console as `Loupedeck.DeviceType`
  `Loupedeck70` (keypad: display, brightness, buttons), `Loupedeck71`
  (dialpad: encoders, `InitializeEncoderSettings`, `UpdateClicksMultiplier`)
  and `Loupedeck72` (image size / scale factor).
- It does not open the console's HID nodes itself.
  `Loupedeck7DeviceManager.StartDeviceWrapper` and
  `ExtendedDevicesMessageHandler` (`ButtonPressed`, `EncoderRotated`,
  `SetDisplayImage`, `Get/SetBrightness`, `GoSleep`, `WakeUp`) receive the
  device traffic as messages from the native Options+ agent.
- Its user strings are encrypted (4 `#US` literals in the whole assembly), so
  static IL reads give structure, not constants.

## Open questions

- Keypad multi-packet image writes: whether, and how, the device acks a
  two-packet JPEG (the observed single-packet ack is `13 ff 02 2b c1 00 01`).

- Dialpad native-mode (`setMode 0`) output: the HID usages produced by the dial
  and the roller when not diverted.
- Keypad `0x8040` value `0` semantics (a reference driver says it resets the
  device; never tried).
- Keypad `0x92e2`, `0x1eb0` (also on the dialpad and other devices) and the
  unsolicited `13 ff 02 10 c5 a0` notification.
