// OpenLogi Frontmost Window — GNOME Shell extension.
//
// Exports a tiny D-Bus service that returns the WM_CLASS of the currently
// focused window and signals focus changes. OpenLogi's `gnome_shell` backend
// uses this to drive per-app mouse-profile switching on GNOME Wayland, where
// the focused window is otherwise not visible to ordinary clients.
//
// It also keeps Actions Ring surfaces out of GNOME's window animations so
// their initial pointer coordinates stay valid. Flow reads cursor position and
// Control state in the unlocked session. ESM module style for GNOME Shell 45+.

import Clutter from 'gi://Clutter';
import Meta from 'gi://Meta';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import {Extension, InjectionManager} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {FlowPointer} from './flowPointer.js';
import {withoutRingAnimation} from './ringAnimation.js';

const DBUS_NAME = 'org.openlogi.Frontmost';
const DBUS_PATH = '/org/openlogi/Frontmost';
const DBUS_INTERFACE = `
<node>
  <interface name="org.openlogi.Frontmost">
    <method name="GetFocusedWmClass">
      <arg type="s" direction="out" name="wmClass"/>
    </method>
    <signal name="FocusedWmClassChanged">
      <arg type="s" name="wmClass"/>
    </signal>
  </interface>
</node>`;

export default class OpenLogiFrontmostExtension extends Extension {
    enable() {
        this._dbus = Gio.DBusExportedObject.wrapJSObject(DBUS_INTERFACE, this);
        this._dbus.export(Gio.DBus.session, DBUS_PATH);
        this._nameId = Gio.bus_own_name_on_connection(
            Gio.DBus.session,
            DBUS_NAME,
            Gio.BusNameOwnerFlags.NONE,
            null,
            null);
        this._focusChangedId = global.display.connect(
            'notify::focus-window',
            () => this._emitFocusedWmClassChanged());

        if (Meta.is_wayland_compositor()) {
            this._flow = new FlowPointer({
                active: () => !Main.sessionMode.isLocked && !Main.sessionMode.isGreeter,
                monitors: () => Main.layoutManager.monitors,
                pointer: () => {
                    const [x, y, mask] = global.get_pointer();
                    return [x, y, Boolean(mask & Clutter.ModifierType.CONTROL_MASK)];
                },
                tracker: () => global.backend?.get_cursor_tracker?.()
                    ?? Meta.CursorTracker.get_for_display(global.display),
                warp: (x, y) => Clutter.get_default_backend().get_default_seat().warp_pointer(x, y),
            });
            this._layoutChanged = Main.layoutManager.connect('monitors-changed', () => this._flow.invalidate());
            this._modeChanged = Main.sessionMode.connect('updated', () => this._flow.invalidate());
        }

        this._ringAnimations = new InjectionManager();
        this._ringAnimations.overrideMethod(
            Main.wm, '_shouldAnimateActor', withoutRingAnimation);
    }

    disable() {
        if (this._flow) {
            Main.layoutManager.disconnect(this._layoutChanged);
            Main.sessionMode.disconnect(this._modeChanged);
            this._flow.destroy();
            this._flow = null;
        }
        this._ringAnimations?.clear();
        this._ringAnimations = null;
        if (this._focusChangedId) {
            global.display.disconnect(this._focusChangedId);
            this._focusChangedId = 0;
        }
        if (this._nameId) {
            Gio.bus_unown_name(this._nameId);
            this._nameId = 0;
        }
        if (this._dbus) {
            this._dbus.unexport();
            this._dbus = null;
        }
    }

    // D-Bus method org.openlogi.Frontmost.GetFocusedWmClass.
    GetFocusedWmClass() {
        const win = global.display.focus_window;
        if (!win)
            return '';
        return win.get_wm_class() || '';
    }

    _emitFocusedWmClassChanged() {
        if (this._dbus) {
            this._dbus.emit_signal(
                'FocusedWmClassChanged',
                new GLib.Variant('(s)', [this.GetFocusedWmClass()]));
        }
    }
}
