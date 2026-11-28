// GNOME scales ordinary Wayland windows while mapping them. Pointer-enter
// coordinates during that effect belong to the transformed surface, but the
// ring keeps its first coordinate as the center after the effect ends. Skip
// the effect for this one transient surface so its coordinate space is stable.
import {isRingWindow} from './ringWindow.js';

export function withoutRingAnimation(original) {
    return function (actor, ...args) {
        if (isRingWindow(actor.meta_window))
            return false;
        return original.call(this, actor, ...args);
    };
}
