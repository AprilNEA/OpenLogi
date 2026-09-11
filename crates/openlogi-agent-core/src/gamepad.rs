//! Opt-in auxiliary virtual-gamepad runtime.
//!
//! The orchestrator publishes the desired set of pads; this handle creates and
//! destroys [`openlogi_gamepad::VirtualGamepad`] instances and applies mapped
//! input from the HID++ capture path.
//!
//! Diversion only engages for keys that currently have a **live** pad — if
//! create fails (no entitlement / no uinput / ViGEm missing), controls stay on
//! the productivity path.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use openlogi_core::binding::{
    ButtonId, DpadDirection, GamepadAxis, GamepadBinding, GamepadFaceButton, GamepadMap,
    GestureDirection,
};
use openlogi_core::device_order::PhysicalDeviceKey;
use openlogi_gamepad::{GamepadState, Rumble, VirtualGamepad, create as create_pad};
use openlogi_hid::DeviceRoute;
use tracing::{debug, info, warn};

/// How long a thumb-wheel axis pulse stays deflected before returning to zero.
const AXIS_PULSE: Duration = Duration::from_millis(50);

/// One online device that should own a virtual pad.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GamepadPadDesired {
    /// Config namespace / display key.
    pub config_key: String,
    /// Stable physical identity used as the pad map key.
    pub physical_key: PhysicalDeviceKey,
    /// Product string shown to the OS / Gamepad API.
    pub product_name: String,
    /// HID++ route for rumble → haptic writes.
    pub route: DeviceRoute,
    /// Whether host rumble should play device haptics.
    pub rumble: bool,
    /// Whether the device reports `haptic_feedback`.
    pub haptic_capable: bool,
}

/// Cloneable apply/sync handle shared by the orchestrator and capture watchers.
#[derive(Clone, Default)]
pub struct GamepadPads {
    inner: Arc<Mutex<GamepadPadsInner>>,
}

#[derive(Default)]
struct GamepadPadsInner {
    pads: HashMap<String, LivePad>,
    /// Optional callback the agent registers for rumble → haptic.
    rumble_sink: Option<Arc<dyn Fn(DeviceRoute, Rumble) + Send + Sync>>,
}

struct LivePad {
    desired: GamepadPadDesired,
    map: GamepadMap,
    state: GamepadState,
    device: Box<dyn VirtualGamepad>,
    /// Axes that should return to zero after [`AXIS_PULSE`].
    axis_hold_until: HashMap<GamepadAxis, Instant>,
}

impl GamepadPads {
    /// Register a sink that receives host rumble for haptic-capable pads.
    pub fn set_rumble_sink<F>(&self, sink: F)
    where
        F: Fn(DeviceRoute, Rumble) + Send + Sync + 'static,
    {
        if let Ok(mut guard) = self.inner.lock() {
            guard.rumble_sink = Some(Arc::new(sink));
        }
    }

    /// Whether a virtual pad is currently live for `config_key`.
    #[must_use]
    pub fn is_active(&self, config_key: &str) -> bool {
        self.inner
            .lock()
            .is_ok_and(|guard| guard.pads.contains_key(config_key))
    }

    /// Diff `desired` against live pads: create, update, destroy.
    pub fn sync(&self, desired: &[GamepadPadDesired]) {
        let Ok(mut guard) = self.inner.lock() else {
            warn!("gamepad pads lock poisoned — sync skipped");
            return;
        };
        let wanted: HashMap<String, GamepadPadDesired> = desired
            .iter()
            .cloned()
            .map(|pad| (pad.config_key.clone(), pad))
            .collect();

        let stale: Vec<String> = guard
            .pads
            .keys()
            .filter(|key| !wanted.contains_key(*key))
            .cloned()
            .collect();
        for key in stale {
            destroy_pad(&mut guard, &key);
        }

        for (key, want) in wanted {
            let recreate = guard
                .pads
                .get(&key)
                .is_some_and(|existing| existing.desired.product_name != want.product_name);
            if recreate {
                destroy_pad(&mut guard, &key);
            } else if let Some(existing) = guard.pads.get_mut(&key) {
                existing.desired = want;
                continue;
            }
            match create_pad(&want.product_name) {
                Ok(device) => {
                    info!(config_key = %key, name = %want.product_name, "created virtual gamepad");
                    guard.pads.insert(
                        key,
                        LivePad {
                            desired: want,
                            map: GamepadMap::default_for_mouse(),
                            state: GamepadState::default(),
                            device,
                            axis_hold_until: HashMap::new(),
                        },
                    );
                }
                Err(error) => {
                    warn!(
                        config_key = %key,
                        %error,
                        "virtual gamepad create failed — feature inactive for this device"
                    );
                }
            }
        }
    }

    /// Whether `config_key`'s live pad owns `button` (actions must not run).
    #[must_use]
    pub fn owns_button(&self, config_key: &str, button: ButtonId) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|guard| {
                guard
                    .pads
                    .get(config_key)
                    .map(|pad| pad.map.owns_button(button))
            })
            .unwrap_or(false)
    }

    /// Apply a face-button edge.
    pub fn set_button(&self, config_key: &str, button: GamepadFaceButton, pressed: bool) {
        self.with_pad(config_key, |pad| {
            pad.state.set_button(button, pressed);
            emit(pad);
        });
    }

    /// Apply a D-pad arm.
    pub fn set_dpad(&self, config_key: &str, direction: DpadDirection, pressed: bool) {
        self.with_pad(config_key, |pad| {
            pad.state.set_dpad(direction, pressed);
            emit(pad);
        });
    }

    /// Apply a continuous axis sample.
    pub fn set_axis(&self, config_key: &str, axis: GamepadAxis, value: f32) {
        self.with_pad(config_key, |pad| {
            pad.state.set_axis(axis, value);
            emit(pad);
        });
    }

    /// Map a physical button edge through the default map.
    pub fn apply_button_edge(&self, config_key: &str, button: ButtonId, pressed: bool) {
        let Some(binding) = self.binding_for(config_key, button) else {
            return;
        };
        match binding {
            GamepadBinding::Button(face) => self.set_button(config_key, face, pressed),
            GamepadBinding::Axis(axis) if !pressed => self.set_axis(config_key, axis, 0.0),
            GamepadBinding::Axis(_) | GamepadBinding::Dpad => {}
        }
    }

    /// Map a gesture swipe/click through the default map.
    pub fn apply_gesture(&self, config_key: &str, button: ButtonId, direction: GestureDirection) {
        let Ok(guard) = self.inner.lock() else {
            return;
        };
        let Some(pad) = guard.pads.get(config_key) else {
            return;
        };
        if direction == GestureDirection::Click {
            if let Some(face) = pad.map.gesture_click(button) {
                drop(guard);
                self.set_button(config_key, face, true);
                self.set_button(config_key, face, false);
            }
            return;
        }
        if let Some(dpad) = pad.map.dpad_direction(button, direction) {
            drop(guard);
            self.set_dpad(config_key, dpad, true);
            self.set_dpad(config_key, dpad, false);
        }
    }

    /// Pulse a thumb-wheel axis, then auto-neutralize after [`AXIS_PULSE`].
    pub fn apply_thumbwheel_axis(&self, config_key: &str, button: ButtonId, magnitude: f32) {
        let Some(binding) = self.binding_for(config_key, button) else {
            return;
        };
        let GamepadBinding::Axis(axis) = binding else {
            return;
        };
        let sign = if button == ButtonId::ThumbwheelScrollDown {
            -1.0
        } else {
            1.0
        };
        let value = (magnitude * sign).clamp(-1.0, 1.0);
        self.with_pad(config_key, |pad| {
            pad.state.set_axis(axis, value);
            pad.axis_hold_until
                .insert(axis, Instant::now() + AXIS_PULSE);
            emit(pad);
        });
    }

    /// Clear all buttons/axes for `config_key` and emit (capture teardown).
    pub fn neutralize(&self, config_key: &str) {
        self.with_pad(config_key, |pad| {
            pad.state = GamepadState::default();
            pad.axis_hold_until.clear();
            emit(pad);
        });
    }

    /// Expire thumb-wheel axis pulses and poll host rumble.
    pub fn tick(&self) {
        self.expire_axis_holds();
        self.poll_rumble();
    }

    /// Poll every live pad for host rumble and forward to the sink.
    pub fn poll_rumble(&self) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let sink = guard.rumble_sink.clone();
        for pad in guard.pads.values_mut() {
            if !(pad.desired.rumble && pad.desired.haptic_capable) {
                let _ = pad.device.poll_rumble();
                continue;
            }
            if let Some(rumble) = pad.device.poll_rumble()
                && rumble.is_active()
                && let Some(sink) = sink.as_ref()
            {
                sink(pad.desired.route.clone(), rumble);
            }
        }
    }

    /// Spawn a background maintenance poller (axis decay + rumble).
    pub fn spawn_rumble_poller(&self) {
        let pads = self.clone();
        thread::Builder::new()
            .name("openlogi-gamepad-tick".into())
            .spawn(move || {
                loop {
                    pads.tick();
                    thread::sleep(Duration::from_millis(30));
                }
            })
            .ok();
    }

    fn expire_axis_holds(&self) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let now = Instant::now();
        for pad in guard.pads.values_mut() {
            let expired: Vec<GamepadAxis> = pad
                .axis_hold_until
                .iter()
                .filter_map(|(axis, until)| (*until <= now).then_some(*axis))
                .collect();
            if expired.is_empty() {
                continue;
            }
            for axis in expired {
                pad.axis_hold_until.remove(&axis);
                pad.state.set_axis(axis, 0.0);
            }
            emit(pad);
        }
    }

    fn binding_for(&self, config_key: &str, button: ButtonId) -> Option<GamepadBinding> {
        self.inner
            .lock()
            .ok()?
            .pads
            .get(config_key)?
            .map
            .button_binding(button)
    }

    fn with_pad(&self, config_key: &str, f: impl FnOnce(&mut LivePad)) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        if let Some(pad) = guard.pads.get_mut(config_key) {
            f(pad);
        }
    }
}

fn destroy_pad(guard: &mut GamepadPadsInner, key: &str) {
    if let Some(pad) = guard.pads.remove(key) {
        info!(config_key = %key, "destroying virtual gamepad");
        if let Err(error) = pad.device.shutdown() {
            warn!(config_key = %key, %error, "virtual gamepad shutdown failed");
        }
    }
}

fn emit(pad: &mut LivePad) {
    if let Err(error) = pad.device.set_state(&pad.state) {
        debug!(
            config_key = %pad.desired.config_key,
            %error,
            "virtual gamepad set_state failed"
        );
    }
}
