// Real GJS D-Bus service with a simulated stage for the Rust pointer worker.
// The Rust harness starts it on a disposable bus, never the desktop bus.
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import {FlowPointer} from '../openlogi-frontmost@openlogi.dev/flowPointer.js';

const wallNow = Date.now;
const monotonicNow = GLib.get_monotonic_time;
let position = [0, 0, false];
const WarpMode = Object.freeze({Delayed: 'delayed', Confined: 'confined', Interrupt: 'interrupt'});
let mode = WarpMode.Delayed;
let reads = 0;
let warps = 0;
let callback = null;
let tracking = 0;
let active = true;
let service = null;
const tracker = {
    connect(_name, listener) { callback = listener; return 1; },
    disconnect() { callback = null; },
    track_position() { tracking++; callback?.(); },
    untrack_position() { tracking--; },
};
const desktop = {
    active: () => active,
    monitors: () => [
        {x: -1280, y: 0, width: 1280, height: 720, scale: 1.5},
        {x: 0, y: 0, width: 1920, height: 1080, scale: 1},
    ],
    pointer() { reads++; return position; },
    tracker: () => tracker,
    warp(x, y) {
        warps++;
        if (mode === WarpMode.Delayed) {
            GLib.idle_add(GLib.PRIORITY_DEFAULT_IDLE, () => {
                position = [x, y, false];
                callback?.();
                return GLib.SOURCE_REMOVE;
            });
        } else if (mode === WarpMode.Interrupt) {
            GLib.idle_add(GLib.PRIORITY_DEFAULT_IDLE, () => {
                service.invalidate();
                return GLib.SOURCE_REMOVE;
            });
        }
    },
};
const panel = Gio.DBusExportedObject.wrapJSObject(`<node><interface name="org.openlogi.Test">
    <method name="Move"><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="b" direction="in"/></method>
    <method name="Mode"><arg type="s" direction="in"/></method>
    <method name="SimulateLateDelivery"><arg type="b" direction="in"/></method>
    <method name="Stats"><arg type="u" direction="out"/><arg type="u" direction="out"/><arg type="i" direction="out"/><arg type="b" direction="out"/></method>
</interface></node>`, {
    Move(x, y, ctrl) { position = [x, y, ctrl]; },
    ModeAsync([value], call) {
        if (value === 'enable') service = new FlowPointer(desktop);
        else if (value === 'disable') { service.destroy(); service = null; }
        else if (value === 'lock' || value === 'unlock') {
            active = value === 'unlock';
            service.invalidate();
        } else if (Object.values(WarpMode).includes(value)) {
            mode = value;
        } else {
            call.return_dbus_error('org.freedesktop.DBus.Error.InvalidArgs', `Unknown fixture mode: ${value}`);
            return;
        }
        call.return_value(null);
    },
    SimulateLateDelivery(enabled) {
        // Only the simulated Shell process sees the time at dispatch. Two seconds
        // passed in the queue and the wall clock stepped back by one minute.
        GLib.get_monotonic_time = enabled ? () => monotonicNow() + 2_000_000 : monotonicNow;
        Date.now = enabled ? () => wallNow() - 60_000 : wallNow;
    },
    Stats() { return [reads, warps, tracking, callback !== null]; },
});
panel.export(Gio.DBus.session, '/org/openlogi/Test');
Gio.bus_own_name_on_connection(Gio.DBus.session, 'org.openlogi.Frontmost',
    Gio.BusNameOwnerFlags.NONE, null, null);
new GLib.MainLoop(null, false).run();
