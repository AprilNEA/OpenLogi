# Lucide battery icons

Six battery glyphs from [Lucide](https://lucide.dev), vendored for the Windows
tray. They are the exact files gpui-component embeds, so they sit correctly
next to the rest of the app's iconography.

The GUI device card does **not** use these — it draws its own battery shape
(`openlogi-desktop/src/ui/battery.rs`), which a 16 px monochrome tray icon
cannot reproduce. What the two surfaces share is the rule for *when* a battery
is in trouble, not the art: both call `BatteryInfo::needs_attention`. See
`openlogi-agent/src/tray_glyph.rs`.

| File | Shown when |
|---|---|
| `battery-charging.svg` | status is `Charging` / `ChargingSlow` |
| `battery-full.svg` | status is `Full`, or level is `Full` |
| `battery-warning.svg` | status is `Error`, or level is `Critical` |
| `battery-low.svg` | the reading needs attention (at or below 20%) |
| `battery-medium.svg` | any other comfortable charge |
| `battery.svg` | level is `Unknown` and the reading is comfortable |

Licensed **ISC** — see `LICENSE`, copied verbatim from the Lucide repository.
Compatible with this project's MIT/Apache-2.0 dual licence. None of these six
are in Lucide's Feather-derived (MIT) subset, so only the ISC terms apply.

These are `stroke="currentColor"` outline icons on a 24x24 viewBox. The tray
recolours them by substituting the stroke colour before parsing, then renders
with `resvg` — see `openlogi-agent/src/tray_icon.rs`.

**Updating:** re-copy from gpui-component's `crates/assets/assets/icons/` so the
tray keeps matching the icon set the rest of the app is drawn from.
