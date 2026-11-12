// Mutter has no layer shell, so the Actions Ring maps as an ordinary window
// and takes keyboard focus. The ring only needs the pointer, but the shortcut
// a slot injects (Ctrl+C, Alt+Left, ...) goes to the focused window: the ring.
// Hand keyboard focus straight back to the window the ring opened over.
// `MetaWindow.focus()` does not raise, so the ring stays on top.
const RING_WM_CLASS = 'openlogi-action-ring';

function isRing(win) {
    return win?.get_wm_class() === RING_WM_CLASS;
}

// `initialFocus` is the window focused when the extension is enabled, so the
// first ring after enabling already has a window to hand focus back to.
export function ringFocusGuard(initialFocus, timestamp) {
    let lastApp = isRing(initialFocus) ? null : initialFocus ?? null;
    return function onFocusWindow(win) {
        if (!win)
            return;
        if (!isRing(win)) {
            lastApp = win;
            return;
        }
        try {
            lastApp?.focus(timestamp());
        } catch {
            // lastApp was unmanaged meanwhile; nothing to give focus back to.
            lastApp = null;
        }
    };
}
