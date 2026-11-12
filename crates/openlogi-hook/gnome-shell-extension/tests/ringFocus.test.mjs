import assert from 'node:assert/strict';
import test from 'node:test';

import {ringFocusGuard} from '../openlogi-frontmost@openlogi.dev/ringFocus.js';

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

const now = () => 42;

test('a ring taking focus hands it back to the window it opened over', () => {
    const editor = window('org.gnome.TextEditor');
    const onFocus = ringFocusGuard(null, now);
    onFocus(editor);
    onFocus(window('openlogi-action-ring'));
    assert.deepEqual(editor.focused, [42]);
});

test('the window focused when the extension starts is handed focus back', () => {
    const browser = window('firefox');
    const onFocus = ringFocusGuard(browser, now);
    onFocus(window('openlogi-action-ring'));
    assert.deepEqual(browser.focused, [42]);
});

test('focus goes back to the latest application, never to a ring', () => {
    const [first, second] = [window('firefox'), window('org.gnome.Nautilus')];
    const ring = window('openlogi-action-ring');
    const onFocus = ringFocusGuard(ring, now);
    onFocus(first);
    onFocus(second);
    onFocus(ring);
    onFocus(window('openlogi-action-ring'));
    assert.deepEqual(first.focused, []);
    assert.deepEqual(second.focused, [42, 42]);
    assert.deepEqual(ring.focused, []);
});

test('ordinary focus changes and losing focus move nothing', () => {
    const [first, second] = [window('firefox'), window('openlogi-action-ring-other')];
    const onFocus = ringFocusGuard(first, now);
    onFocus(null);
    onFocus(second);
    onFocus(first);
    assert.deepEqual(first.focused, []);
    assert.deepEqual(second.focused, []);
});

test('a ring with no application to return to does nothing', () => {
    const onFocus = ringFocusGuard(null, now);
    assert.doesNotThrow(() => onFocus(window('openlogi-action-ring')));
});

test('a window that went away is forgotten instead of throwing', () => {
    const gone = window('firefox');
    gone.focus = () => {
        throw new Error('window unmanaged');
    };
    const onFocus = ringFocusGuard(gone, now);
    assert.doesNotThrow(() => onFocus(window('openlogi-action-ring')));
    assert.doesNotThrow(() => onFocus(window('openlogi-action-ring')));
});
