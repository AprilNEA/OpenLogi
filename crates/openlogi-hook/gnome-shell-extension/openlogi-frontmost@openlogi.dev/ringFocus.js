// Mutter has no layer shell, so the Actions Ring maps as an ordinary window
// and takes keyboard focus. The ring only needs the pointer, but the shortcut
// a slot injects (Ctrl+C, Alt+Left, ...) goes to the focused window: the ring.
// Hand keyboard focus straight back to the window the ring opened over.
// `MetaWindow.focus()` does not raise, so the ring stays on top.
import {isRingWindow} from './ringWindow.js';

// `initialFocus` is the window focused when the extension is enabled, so the
// first ring after enabling already has a window to hand focus back to.
// The returned handler reports whether it handed focus back, so the caller
// never publishes the ring as the focused window.
export function ringFocusGuard(initialFocus, timestamp) {
    let lastApp = isRingWindow(initialFocus) ? null : initialFocus;
    return function onFocusWindow(win) {
        if (!win)
            return false;
        if (!isRingWindow(win)) {
            lastApp = win;
            return false;
        }
        try {
            lastApp?.focus(timestamp());
        } catch {
            // lastApp was unmanaged meanwhile; nothing to give focus back to.
            lastApp = null;
        }
        return true;
    };
}
