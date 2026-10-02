//! A plugin owns one cancellation token for queued and active input.

use super::{
    Action, ActionDispatchTarget, Arc, AtomicBool, AtomicUsize, ButtonCommand, ButtonInput,
    ButtonInputHandle, ButtonSource, CapabilityId, ControlId, Instant, Ordering,
    PERIPHERAL_QUEUE_CAPACITY, PressBehavior, PressControl, PressKey, SessionId,
};
use openlogi_core::peripheral::InputTransition;

#[derive(Clone, Debug)]
pub(super) struct Validity(Arc<AtomicBool>);

impl PartialEq for Validity {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for Validity {}

impl Validity {
    pub(super) fn alive(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub(super) struct Permit(Arc<AtomicUsize>);

impl Permit {
    fn acquire(count: &Arc<AtomicUsize>) -> Option<Self> {
        count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                (value < PERIPHERAL_QUEUE_CAPACITY).then_some(value + 1)
            })
            .ok()?;
        Some(Self(Arc::clone(count)))
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) struct PeripheralInput {
    input: ButtonInputHandle,
    session: SessionId,
    validity: Validity,
}

impl PeripheralInput {
    pub(super) fn new(input: ButtonInputHandle, session: SessionId) -> Self {
        Self {
            input,
            session,
            validity: Validity(Arc::new(AtomicBool::new(true))),
        }
    }

    pub(crate) fn send(
        &self,
        capability: &CapabilityId,
        control: &ControlId,
        transition: InputTransition,
        action: &Action,
    ) -> bool {
        if !self.validity.alive() || !self.input.accepting.load(Ordering::Acquire) {
            return false;
        }
        let Some(permit) = Permit::acquire(&self.input.peripheral_inflight) else {
            self.cancel();
            return false;
        };
        let generation = self.input.generation.load(Ordering::Acquire);
        let key = PressKey {
            source: ButtonSource::Peripheral(self.session.clone()),
            control: PressControl::Peripheral(capability.clone(), control.clone()),
        };
        let input = match transition {
            InputTransition::Release => ButtonInput::Up {
                key,
                released_at: Instant::now(),
            },
            InputTransition::Press | InputTransition::Trigger => {
                let mut press = self.input.new_press(
                    key,
                    PressBehavior::Immediate(action.clone()),
                    generation,
                    ActionDispatchTarget::capture(),
                );
                press.validity = Some(self.validity.clone());
                if transition == InputTransition::Trigger {
                    ButtonInput::Pulse(press)
                } else {
                    ButtonInput::Down(press)
                }
            }
        };
        if self
            .input
            .events
            .try_send(ButtonCommand::Peripheral {
                generation,
                input,
                validity: self.validity.clone(),
                permit,
            })
            .is_err()
        {
            self.cancel();
            return false;
        }
        true
    }

    pub(crate) fn cancel(&self) {
        self.validity.0.store(false, Ordering::Release);
        // A full queue already wakes the worker. Cancellation must not need a queue slot.
        let _ = self.input.events.try_send(ButtonCommand::Wake);
    }
}

impl Drop for PeripheralInput {
    fn drop(&mut self) {
        self.cancel();
    }
}
