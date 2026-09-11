//! Linux uinput gamepad backend.

use std::time::{SystemTime, UNIX_EPOCH};

use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, EventType, InputEvent, KeyCode, UinputAbsSetup,
    uinput::VirtualDevice,
};

use crate::{GamepadError, GamepadState, Rumble, VirtualGamepad};

const DEVICE_NAME_PREFIX: &str = "OpenLogi Virtual Gamepad";

/// Create a uinput joystick that browsers/SDL see as a gamepad.
pub fn create(product_name: &str) -> Result<Box<dyn VirtualGamepad>, GamepadError> {
    let name = format!("{DEVICE_NAME_PREFIX} ({product_name})");
    let device = build(&name)?;
    Ok(Box::new(LinuxGamepad { device }))
}

struct LinuxGamepad {
    device: VirtualDevice,
}

fn build(name: &str) -> Result<VirtualDevice, GamepadError> {
    let mut keys = AttributeSet::<KeyCode>::default();
    for code in [
        KeyCode::BTN_SOUTH,
        KeyCode::BTN_EAST,
        KeyCode::BTN_NORTH,
        KeyCode::BTN_WEST,
        KeyCode::BTN_TL,
        KeyCode::BTN_TR,
        KeyCode::BTN_SELECT,
        KeyCode::BTN_START,
        KeyCode::BTN_MODE,
        KeyCode::BTN_THUMBL,
        KeyCode::BTN_THUMBR,
    ] {
        keys.insert(code);
    }

    let stick = AbsInfo::new(0, -32767, 32767, 16, 128, 0);
    let trigger = AbsInfo::new(0, 0, 255, 0, 0, 0);
    let hat = AbsInfo::new(0, -1, 1, 0, 0, 0);

    // Do not advertise FF_RUMBLE until poll_rumble reads uinput force-feedback
    // events — advertising without implementing leaves host rumble dead.
    Ok(VirtualDevice::builder()?
        .name(name)
        .with_keys(&keys)?
        .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_X, stick))?
        .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_Y, stick))?
        .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_RX, stick))?
        .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_RY, stick))?
        .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_Z, trigger))?
        .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_RZ, trigger))?
        .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_HAT0X, hat))?
        .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_HAT0Y, hat))?
        .build()?)
}

impl VirtualGamepad for LinuxGamepad {
    fn set_state(&mut self, state: &GamepadState) -> Result<(), GamepadError> {
        let now = event_time();
        let mut events = Vec::with_capacity(20);
        push_key(
            &mut events,
            now,
            KeyCode::BTN_SOUTH,
            state.button_pressed(openlogi_core::binding::GamepadFaceButton::A),
        );
        push_key(
            &mut events,
            now,
            KeyCode::BTN_EAST,
            state.button_pressed(openlogi_core::binding::GamepadFaceButton::B),
        );
        push_key(
            &mut events,
            now,
            KeyCode::BTN_WEST,
            state.button_pressed(openlogi_core::binding::GamepadFaceButton::X),
        );
        push_key(
            &mut events,
            now,
            KeyCode::BTN_NORTH,
            state.button_pressed(openlogi_core::binding::GamepadFaceButton::Y),
        );
        push_key(
            &mut events,
            now,
            KeyCode::BTN_TL,
            state.button_pressed(openlogi_core::binding::GamepadFaceButton::LeftShoulder),
        );
        push_key(
            &mut events,
            now,
            KeyCode::BTN_TR,
            state.button_pressed(openlogi_core::binding::GamepadFaceButton::RightShoulder),
        );
        push_key(
            &mut events,
            now,
            KeyCode::BTN_SELECT,
            state.button_pressed(openlogi_core::binding::GamepadFaceButton::Select),
        );
        push_key(
            &mut events,
            now,
            KeyCode::BTN_START,
            state.button_pressed(openlogi_core::binding::GamepadFaceButton::Start),
        );
        push_abs(
            &mut events,
            now,
            AbsoluteAxisCode::ABS_X,
            axis_i32(state.left_x),
        );
        push_abs(
            &mut events,
            now,
            AbsoluteAxisCode::ABS_Y,
            axis_i32(state.left_y),
        );
        push_abs(
            &mut events,
            now,
            AbsoluteAxisCode::ABS_RX,
            axis_i32(state.right_x),
        );
        push_abs(
            &mut events,
            now,
            AbsoluteAxisCode::ABS_RY,
            axis_i32(state.right_y),
        );
        push_abs(
            &mut events,
            now,
            AbsoluteAxisCode::ABS_Z,
            trigger_i32(state.left_trigger),
        );
        push_abs(
            &mut events,
            now,
            AbsoluteAxisCode::ABS_RZ,
            trigger_i32(state.right_trigger),
        );
        push_abs(
            &mut events,
            now,
            AbsoluteAxisCode::ABS_HAT0X,
            i32::from(state.dpad_x),
        );
        push_abs(
            &mut events,
            now,
            AbsoluteAxisCode::ABS_HAT0Y,
            i32::from(state.dpad_y),
        );
        events.push(InputEvent::new_now(EventType::SYNCHRONIZATION.0, 0, 0));
        self.device.emit(&events)?;
        Ok(())
    }

    fn poll_rumble(&mut self) -> Option<Rumble> {
        // Do not advertise FF until we read uinput force-feedback events.
        None
    }

    fn shutdown(self: Box<Self>) -> Result<(), GamepadError> {
        drop(self.device);
        Ok(())
    }
}

fn push_key(events: &mut Vec<InputEvent>, time: evdev::SystemTime, code: KeyCode, pressed: bool) {
    events.push(InputEvent::new(
        time,
        EventType::KEY.0,
        code.code(),
        i32::from(pressed),
    ));
}

fn push_abs(
    events: &mut Vec<InputEvent>,
    time: evdev::SystemTime,
    code: AbsoluteAxisCode,
    value: i32,
) {
    events.push(InputEvent::new(time, EventType::ABSOLUTE.0, code.0, value));
}

fn axis_i32(value: f32) -> i32 {
    let scaled = (value.clamp(-1.0, 1.0) * 32767.0).round();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "scaled is within i16 after clamp"
    )]
    {
        scaled as i32
    }
}

fn trigger_i32(value: f32) -> i32 {
    let scaled = (value.clamp(0.0, 1.0) * 255.0).round();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "scaled is within 0..=255"
    )]
    {
        scaled as i32
    }
}

fn event_time() -> evdev::SystemTime {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .into()
}
