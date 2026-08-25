# Lucide battery icons

Six battery glyphs from [Lucide](https://lucide.dev), vendored so the Windows
tray can draw the **same** icons the GUI already shows on its device cards
(`openlogi-desktop/src/app/home.rs::battery_icon`). They are the exact files
gpui-component embeds, so the two surfaces cannot drift apart visually.

| File | Shown when |
|---|---|
| `battery-charging.svg` | status is `Charging` / `ChargingSlow` |
| `battery-full.svg` | status is `Full`, or level is `Full` |
| `battery-warning.svg` | status is `Error`, or level is `Critical` |
| `battery-low.svg` | level is `Low` |
| `battery-medium.svg` | level is `Good` |
| `battery.svg` | level is `Unknown` |

Licensed **ISC** — see `LICENSE`, copied verbatim from the Lucide repository.
Compatible with this project's MIT/Apache-2.0 dual licence. None of these six
are in Lucide's Feather-derived (MIT) subset, so only the ISC terms apply.

These are `stroke="currentColor"` outline icons on a 24x24 viewBox. The tray
recolours them by substituting the stroke colour before parsing, then renders
with `resvg` — see `openlogi-agent/src/tray_icon.rs`.

**Updating:** re-copy from gpui-component's `crates/assets/assets/icons/` so the
tray and the GUI keep rendering identical art.
