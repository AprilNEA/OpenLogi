// The Actions Ring overlay's Wayland app id, which Mutter reports as its
// WM_CLASS. Keep in sync with `RING_APP_ID` in crates/openlogi-overlay/src/ring.rs.
export const RING_WM_CLASS = 'openlogi-action-ring';

export function isRingWindow(win) {
    return win?.get_wm_class() === RING_WM_CLASS;
}
