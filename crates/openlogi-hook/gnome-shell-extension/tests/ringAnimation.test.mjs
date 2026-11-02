import assert from 'node:assert/strict';
import test from 'node:test';

import {withoutRingAnimation} from '../openlogi-frontmost@openlogi.dev/ringAnimation.js';

function actor(appId) {
    return {meta_window: {get_wm_class: () => appId}};
}

test('the Actions Ring never receives a surface-transforming window effect', () => {
    const shouldAnimate = withoutRingAnimation(() => true);
    assert.equal(shouldAnimate(actor('openlogi-action-ring'), ['normal']), false);
});

test('ring suppression does not call the ordinary animation policy', () => {
    const shouldAnimate = withoutRingAnimation(() => {
        assert.fail('the ring must not enter the ordinary animation policy');
    });
    assert.equal(shouldAnimate(actor('openlogi-action-ring'), ['normal']), false);
});

test('other windows preserve the original receiver, arguments, and decision', () => {
    const manager = {animate: true};
    const window = actor('org.gnome.TextEditor');
    const types = ['normal', 'dialog'];
    const shouldAnimate = withoutRingAnimation(function (...args) {
        assert.equal(this, manager);
        assert.deepEqual(args, [window, types]);
        return this.animate;
    });
    assert.equal(shouldAnimate.call(manager, window, types), true);
    manager.animate = false;
    assert.equal(shouldAnimate.call(manager, window, types), false);
});

test('matching is exact and leaves other OpenLogi windows and unknown classes alone', () => {
    const shouldAnimate = withoutRingAnimation(() => true);
    for (const appId of [null, '', 'openlogi', 'openlogi-notice', 'openlogi-action-ring-other'])
        assert.equal(shouldAnimate(actor(appId), ['normal']), true);
});
