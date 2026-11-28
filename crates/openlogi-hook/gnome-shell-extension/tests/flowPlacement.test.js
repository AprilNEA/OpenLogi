// Run with gjs -m. Uses the production placement controller without a Shell session.
import GLib from 'gi://GLib';
import {CursorPlacement} from '../openlogi-frontmost@openlogi.dev/flowPointer.js';

function assert(condition, message) {
    if (!condition)
        throw new Error(message);
}

function fixture() {
    const listeners = new Map();
    let serial = 0;
    const state = {
        position: [0, 0],
        reads: 0,
        warps: 0,
        tracking: 0,
        onTrack() {},
        onWarp() {},
    };
    const tracker = {
        connect(_signal, cb) {
            listeners.set(++serial, cb);
            return serial;
        },
        disconnect(id) {
            assert(listeners.delete(id), 'signal disconnected more than once');
        },
        track_position() {
            state.tracking++;
            state.onTrack(listeners.get(serial));
        },
        untrack_position() {
            assert(state.tracking > 0, 'tracking released more than once');
            state.tracking--;
        },
    };
    const placement = new CursorPlacement({
        monitors: () => [{x: 0, y: 0, width: 100, height: 100}],
        tracker: () => tracker,
        pointer() { state.reads++; return state.position; },
        warp() { state.warps++; state.onWarp(); },
    });
    return {
        state, placement, listeners,
        start() {
            const replies = [];
            placement.place(10, 12, GLib.get_monotonic_time() + 500_000, ok => replies.push(ok));
            return replies;
        },
        assertReleased() {
            assert(listeners.size === 0 && state.tracking === 0, 'placement retained tracker resources');
        },
    };
}

function synchronousNotifications() {
    const f = fixture();
    f.state.position = [10, 12];
    let notify;
    f.state.onTrack = cb => {
        notify = cb;
        cb();
        assert(f.state.reads === 0, 'notification sampled the target before warp');
    };
    f.state.onWarp = () => notify();
    const replies = f.start();
    assert(f.state.warps === 1 && f.state.reads === 1, 'synchronous warp must confirm once');
    notify();
    f.placement.cancel();
    assert(replies.length === 1 && replies[0], 'finished request replied twice or failed');
    f.assertReleased();
}

function supersededCallback() {
    const f = fixture();
    const first = f.start();
    const [oldNotify] = f.listeners.values();
    const second = f.start();
    assert(first.length === 1 && !first[0], 'superseded request should fail once');
    const [notify] = f.listeners.values();
    f.state.position = [10, 12];
    oldNotify();
    assert(second.length === 0 && f.listeners.size === 1 && f.state.tracking === 1,
        'stale callback completed or released the successor');
    notify();
    assert(second.length === 1 && second[0], 'successor should still confirm');
    f.assertReleased();
}

function cancellationDuringSetup() {
    const f = fixture();
    f.state.onTrack = () => f.placement.cancel();
    const replies = f.start();
    assert(replies.length === 1 && !replies[0], 'setup cancellation should fail once');
    assert(f.state.warps === 0, 'cancelled preparation issued a warp');
    f.assertReleased();
}

synchronousNotifications();
supersededCallback();
cancellationDuringSetup();
