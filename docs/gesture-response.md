# Directional gesture response

Directional gestures share one raw-distance recognizer across HID++ and OS hooks.
The first deliberate swipe and changes of direction qualify at 50 raw units;
there is no initial hold timer. Same-direction ordinary repeats require another
600 units and at least 400 ms on macOS/Linux or 200 ms on Windows. A 250 ms rest
rearms the distance threshold without bypassing action pacing. Reversals and
axis changes bypass the previous direction's repeat cooldown.

Backtracking and cross-axis drift remain part of recognition. Movement during a
cooldown is bounded to one pending repeat; output requires a fresh nonzero report
continuing that direction. There are no stationary repeats or scheduled bursts.

A stationary or sub-threshold hold clicks on release, even after a long hold.
A deliberate swipe consumes the click, including an unbound direction or an
explicit None action. Both capture paths ignore unbound directions.

This intentionally differs from exact Logi Options+ recognition: response uses
raw distance rather than a per-report power curve, deliberate direction changes
remain responsive during cooldown, and long stationary holds retain clicking.
The behavior does not introduce persisted configuration or IPC fields.

Physical hold identity is retained across pointer/context changes with unchanged
bindings. Each gesture action resolves the current target. Binding changes,
source loss, cancellation and capture retirement terminate gesture output;
independent wheel holds retain their own physical lifecycle.

## Continuous volume

Directions explicitly bound to Volume Up or Volume Down repeat per 50 raw units,
with at least 20 ms between same-direction outputs on macOS/Linux or 10 ms on
Windows. Fractional distance is retained; at most one step is emitted per new
report. A pause produces no output and release clears the hold. Reversals discard
old-direction progress and bypass its cooldown. Other axes and mixed non-volume
bindings continue to use the shared recognizer and their ordinary response.

Related work: #365 describes the one-step volume limitation. #1752 independently
implements movement-driven continuation. This implementation keeps the general
recognizer active after the first swipe, preserving #1652's direction chaining.
