// OpenLogi Frontmost Window — KWin script.
//
// Pushes the focused window's resourceClass to OpenLogi over D-Bus, so per-app
// mouse profiles can follow the active application on Plasma Wayland.
//
// KWin exposes no way for an ordinary client to read the active window:
// `getWindowInfo` needs a UUID nothing hands out, and `queryWindowInfo` asks
// the user to click. A script can see it, but a KWin script cannot export a
// D-Bus service — only call one. So OpenLogi serves the endpoint and this
// pushes to it, the opposite direction from the GNOME Shell extension.
//
// It reads only `resourceClass` — no titles, no window contents, no input.

const SERVICE = "org.openlogi.KWinFrontmost";
const PATH = "/org/openlogi/KWinFrontmost";
const IFACE = "org.openlogi.KWinFrontmost";

function push(window) {
    // Empty string means "nothing focused"; OpenLogi maps it back to no app
    // rather than to an application whose name is blank.
    const cls = window && window.resourceClass ? window.resourceClass : "";
    callDBus(SERVICE, PATH, IFACE, "SetFocusedWindowClass", cls);
}

// Push once at load so a session that starts with a window already focused
// does not wait for the next activation to report anything.
push(workspace.activeWindow);
workspace.windowActivated.connect(push);
