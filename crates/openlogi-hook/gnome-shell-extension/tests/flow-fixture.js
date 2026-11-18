// Real GJS D-Bus service with a simulated stage for the Rust pointer worker.
// The Rust harness starts it on a disposable bus, never the desktop bus.
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import {FlowPointer} from '../openlogi-frontmost@openlogi.dev/flowPointer.js';

let position = [0, 0, false];
let mode = 'delayed';
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
        if (mode === 'delayed') {
            GLib.idle_add(GLib.PRIORITY_DEFAULT_IDLE, () => {
                position = [x, y, false];
                callback?.();
                return GLib.SOURCE_REMOVE;
            });
        } else if (mode === 'interrupt') {
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
    <method name="Stats"><arg type="u" direction="out"/><arg type="u" direction="out"/><arg type="i" direction="out"/><arg type="b" direction="out"/></method>
</interface></node>`, {
    Move(x, y, ctrl) { position = [x, y, ctrl]; },
    Mode(value) {
        if (value === 'enable') service = new FlowPointer(desktop);
        else if (value === 'disable') { service.destroy(); service = null; }
        else if (value === 'lock' || value === 'unlock') {
            active = value === 'unlock';
            service.invalidate();
        } else mode = value;
    },
    Stats() { return [reads, warps, tracking, callback !== null]; },
});
panel.export(Gio.DBus.session, '/org/openlogi/Test');
Gio.bus_own_name_on_connection(Gio.DBus.session, 'org.openlogi.Frontmost',
    Gio.BusNameOwnerFlags.NONE, null, null);
new GLib.MainLoop(null, false).run();
