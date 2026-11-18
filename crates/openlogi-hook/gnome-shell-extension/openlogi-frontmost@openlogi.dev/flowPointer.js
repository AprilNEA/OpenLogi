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

const Phase = Object.freeze({
    Idle: 'idle',
    Preparing: 'preparing',
    Waiting: 'waiting',
    Finished: 'finished',
});

export class CursorPlacement {
    #state = {phase: Phase.Idle};

    constructor(desktop) {
        this._desktop = desktop;
    }

    // Compare the exact state object: a callback from an earlier request can
    // run while another request is waiting. Retire ownership before cleanup,
    // as releasing tracking can itself deliver a position notification.
    #transition(from, to, confirmed = false) {
        if (this.#state !== from)
            return false;
        this.#state = to;
        if (from.phase === Phase.Waiting)
            from.timer.destroy();
        if (from.request && from.request !== to.request) {
            for (const release of from.request.cleanup.reverse()) {
                try {
                    release();
                } catch (error) {
                    console.debug(`OpenLogi Flow tracker cleanup failed: ${error.message}`);
                }
            }
            from.request.reply(confirmed);
        }
        return this.#state === to;
    }

    cancel() {
        const current = this.#state;
        if (current.phase === Phase.Preparing || current.phase === Phase.Waiting)
            this.#transition(current, {phase: Phase.Finished});
    }

    place(targetX, targetY, expiresUs, reply) {
        const x = Math.round(targetX);
        const y = Math.round(targetY);
        const now = GLib.get_monotonic_time();
        const deadline = Math.min(expiresUs, now + 500_000);
        if (deadline <= now || !this._desktop.monitors().some(m =>
            x >= m.x && x < m.x + m.width && y >= m.y && y < m.y + m.height)) {
            reply(false);
            return;
        }

        const request = {reply, cleanup: []};
        const preparing = {phase: Phase.Preparing, request};
        if (!this.#transition(this.#state, preparing))
            return;

        // A synchronous callback may cancel during acquisition. In that case
        // the resource just returned belongs to the abandoned request.
        const keep = release => {
            if (this.#state !== preparing) {
                release();
                return false;
            }
            request.cleanup.push(release);
            return true;
        };
        const check = () => {
            const current = this.#state;
            if (current.phase !== Phase.Waiting || current.request !== request)
                return;
            const [actualX, actualY] = this._desktop.pointer();
            if (actualX === x && actualY === y)
                this.#transition(current, {phase: Phase.Finished}, GLib.get_monotonic_time() < deadline);
        };

        try {
            const tracker = this._desktop.tracker();
            const signal = tracker.connect('position-invalidated', check);
            if (!keep(() => tracker.disconnect(signal)))
                return;
            if (tracker.track_position && tracker.untrack_position) {
                tracker.track_position();
                if (!keep(() => tracker.untrack_position()))
                    return;
            }
            const timeoutMs = Math.max(1, Math.ceil((deadline - GLib.get_monotonic_time()) / 1000));
            const timer = GLib.timeout_source_new(timeoutMs);
            const waiting = {phase: Phase.Waiting, request, timer};
            timer.set_callback(() => {
                this.#transition(waiting, {phase: Phase.Finished});
                return GLib.SOURCE_REMOVE;
            });
            timer.attach(null);
            if (GLib.get_monotonic_time() >= deadline) {
                timer.destroy();
                this.#transition(preparing, {phase: Phase.Finished});
                return;
            }
            if (!this.#transition(preparing, waiting)) {
                timer.destroy();
                return;
            }
            this._desktop.warp(x, y);
            check();
        } catch (error) {
            console.debug(`OpenLogi Flow placement failed: ${error.message}`);
            if (this.#state.request === request)
                this.cancel();
        }
    }
}

export class FlowPointer {
    constructor(desktop) {
        this._desktop = desktop;
        this._owner = null;
        this._placement = new CursorPlacement(desktop);
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
        this._placement.cancel();
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
        this._placement.place(targetX, targetY, expiresUs,
            ok => call.return_value(new GLib.Variant('(b)', [ok])));
    }
}
