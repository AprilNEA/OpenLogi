# Thumb-wheel zoom

The Zoom presets keep continuous native magnification on macOS. On Linux and Windows they emit Control-wheel zoom. Application support varies.

The presets marked **(Shortcut)** use the keyboard zoom bindings from #1590: Cmd+= / Cmd+- on macOS, Ctrl+= / Ctrl+- on Linux and Windows. They are discrete actions with the existing thumb-wheel sensitivity, threshold and cooldown. Use them when an application responds better to its zoom commands than to pinch magnification or Control-wheel events.

Both modes have forward and reversed presets. Existing Zoom bindings keep their behavior. Shortcut zoom is also available as individual actions for buttons and other bindings.

The shortcut alternative incorporates the approach contributed by @johnbuzz98 in #1590. Native magnification, continuous motion response and gesture cleanup remain part of #1743.
