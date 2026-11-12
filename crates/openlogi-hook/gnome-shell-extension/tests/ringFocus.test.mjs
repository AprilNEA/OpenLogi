import assert from 'node:assert/strict';
import test from 'node:test';

import {ringFocusGuard} from '../openlogi-frontmost@openlogi.dev/ringFocus.js';
import {RING_WM_CLASS} from '../openlogi-frontmost@openlogi.dev/ringWindow.js';

function window(wmClass) {
    const win = {
        focused: [],
        get_wm_class: () => wmClass,
        focus(timestamp) {
            win.focused.push(timestamp);
        },
    };
    return win;
}

const ring = () => window(RING_WM_CLASS);
const now = () => 42;

test('a ring taking focus hands it back to the window it opened over', () => {
    const editor = window('org.gnome.TextEditor');
    const onFocus = ringFocusGuard(null, now);
    assert.equal(onFocus(editor), false);
    assert.equal(onFocus(ring()), true, 'the caller does not publish the ring');
    assert.deepEqual(editor.focused, [42]);
});

test('the window focused when the extension starts is handed focus back', () => {
    const browser = window('firefox');
    const onFocus = ringFocusGuard(browser, now);
    onFocus(ring());
    assert.deepEqual(browser.focused, [42]);
});

test('focus goes back to the latest application, never to a ring', () => {
    const first = window('firefox');
    const second = window('org.gnome.Nautilus');
    const startRing = ring();
    const onFocus = ringFocusGuard(startRing, now);
    onFocus(first);
    onFocus(second);
    onFocus(ring());
    onFocus(ring());
    assert.deepEqual(first.focused, []);
    assert.deepEqual(second.focused, [42, 42]);
    assert.deepEqual(startRing.focused, []);
});

test('ordinary focus changes and losing focus move nothing', () => {
    const first = window('firefox');
    const second = window(`${RING_WM_CLASS}-other`);
    const onFocus = ringFocusGuard(first, now);
    assert.equal(onFocus(null), false);
    assert.equal(onFocus(second), false);
    assert.equal(onFocus(first), false);
    assert.deepEqual(first.focused, []);
    assert.deepEqual(second.focused, []);
});

test('a ring with no application to return to does nothing', () => {
    const onFocus = ringFocusGuard(null, now);
    assert.doesNotThrow(() => onFocus(ring()));
});

test('a window that went away is forgotten instead of retried', () => {
    const gone = window('firefox');
    gone.focus = timestamp => {
        gone.focused.push(timestamp);
        throw new Error('window unmanaged');
    };
    const onFocus = ringFocusGuard(gone, now);
    assert.doesNotThrow(() => onFocus(ring()));
    assert.doesNotThrow(() => onFocus(ring()));
    assert.deepEqual(gone.focused, [42], 'a failed target is not retried');
});
