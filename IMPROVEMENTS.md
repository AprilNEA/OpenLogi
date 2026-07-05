# Improvements

- Add a `--device` selector to `openlogi diag features`; `diag controls` already has one, but feature dumps cannot currently isolate one paired device when several are connected.
- Implement the G502 onboard/gaming-button protocol before promoting the currently read-only DPI Up/Down, DPI Shift, Smart Shift, wheel tilt, profile cycle, G-Shift, or other non-standard controls to editable runtime-dispatched buttons.
- Add a cross-platform hook-event diagnostic that prints raw mouse button numbers/codes so G502 extra buttons can be verified on macOS, Linux, and Windows without guessing.
- Extend the asset pipeline/index for G502-family renders and hotspot metadata if the current OpenLogi asset cache does not include those depots.
