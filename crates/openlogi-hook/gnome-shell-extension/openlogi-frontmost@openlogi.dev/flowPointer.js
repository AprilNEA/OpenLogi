import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

const INTERFACE = `<node><interface name="org.openlogi.FlowPointer">
    <method name="GetLayout">
        <arg type="s" direction="out"/><arg type="a(iiii)" direction="out"/>
    </method>
    <method name="ReadPointer">
        <arg type="s" direction="in"/>
        <arg type="i" direction="out"/><arg type="i" direction="out"/><arg type="b" direction="out"/>
    </method>
    <method name="PlacePointerMonotonic">
        <arg type="s" direction="in"/><arg type="d" direction="in"/><arg type="d" direction="in"/>
        <arg type="t" direction="in"/><arg type="b" direction="out"/>
    </method>
</interface></node>`;

export class FlowPointer {
    constructor(desktop) {
        this._desktop = desktop;
        this._owner = null;
        this.invalidate();
        this._object = Gio.DBusExportedObject.wrapJSObject(INTERFACE, this);
        this._object.export(Gio.DBus.session, '/org/openlogi/FlowPointer');
        this._watch = Gio.bus_watch_name_on_connection(Gio.DBus.session,
            'org.openlogi.OpenLogi.Flow', Gio.BusNameWatcherFlags.NONE,
            (_bus, _name, owner) => this._changeOwner(owner),
            () => this._changeOwner(null));
    }

    _changeOwner(owner) {
        this._owner = owner;
        this.invalidate();
    }

    invalidate() {
        this._finish?.(false);
        this._generation = GLib.uuid_string_random();
    }

    destroy() {
        Gio.bus_unwatch_name(this._watch);
        this._changeOwner(null);
        this._object.unexport();
    }

    _allowed(call, generation = this._generation) {
        if (call.get_sender() !== this._owner || !this._desktop.active()) {
            call.return_dbus_error('org.freedesktop.DBus.Error.AccessDenied', 'Flow pointer unavailable');
            return false;
        }
        if (generation !== this._generation) {
            call.return_dbus_error('org.openlogi.Error.LayoutChanged', 'Flow layout changed');
            return false;
        }
        return true;
    }

    GetLayoutAsync(_args, call) {
        if (this._allowed(call)) {
            const rectangles = this._desktop.monitors().map(({x, y, width, height}) =>
                [x, y, width, height]);
            call.return_value(new GLib.Variant('(sa(iiii))', [this._generation, rectangles]));
        }
    }

    ReadPointerAsync([generation], call) {
        if (this._allowed(call, generation))
            call.return_value(new GLib.Variant('(iib)', this._desktop.pointer()));
    }

    PlacePointerMonotonicAsync([generation, targetX, targetY, expiresUs], call) {
        if (!this._allowed(call, generation))
            return;
        const x = Math.round(targetX);
        const y = Math.round(targetY);
        const now = GLib.get_monotonic_time();
        // Limit Shell work even when a caller supplies a distant deadline.
        const deadline = Math.min(expiresUs, now + 500_000);
        const reply = ok => call.return_value(new GLib.Variant('(b)', [ok]));
        if (deadline <= now || !this._desktop.monitors().some(m =>
            x >= m.x && x < m.x + m.width && y >= m.y && y < m.y + m.height)) {
            reply(false);
            return;
        }

        this._finish?.(false);
        let signal = 0;
        let timer = 0;
        let tracking = false;
        let issued = false;
        let tracker;
        const finish = ok => {
            if (this._finish !== finish)
                return;
            this._finish = null;
            if (signal)
                tracker.disconnect(signal);
            if (tracking)
                tracker.untrack_position();
            if (timer)
                GLib.Source.remove(timer);
            const confirmed = ok && GLib.get_monotonic_time() < deadline;
            reply(confirmed);
        };
        this._finish = finish;
        const check = () => {
            if (!issued)
                return;
            const [actualX, actualY] = this._desktop.pointer();
            if (actualX === x && actualY === y)
                finish(true);
        };

        try {
            tracker = this._desktop.tracker();
            // Mutter can queue the warp on its input thread. Subscribe before
            // issuing it and also check once for synchronous completion.
            signal = tracker.connect('position-invalidated', check);
            if (tracker.track_position && tracker.untrack_position) {
                tracker.track_position();
                tracking = true;
            }
            const timeoutMs = Math.max(1, Math.ceil((deadline - GLib.get_monotonic_time()) / 1000));
            timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, timeoutMs, () => {
                timer = 0;
                finish(false);
                return GLib.SOURCE_REMOVE;
            });
            if (GLib.get_monotonic_time() >= deadline) {
                finish(false);
                return;
            }
            issued = true;
            this._desktop.warp(x, y);
            check();
        } catch (error) {
            console.debug(`OpenLogi Flow placement failed: ${error.message}`);
            finish(false);
        }
    }
}
