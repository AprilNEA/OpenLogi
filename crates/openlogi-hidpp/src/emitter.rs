use std::sync::Mutex;

use crate::sync::lock;

/// A simple event emitter sending a single event to multiple MPSC channels.
#[derive(Debug)]
pub struct EventEmitter<T: Clone> {
    senders: Mutex<Vec<async_channel::Sender<T>>>,
}

impl<T: Clone> EventEmitter<T> {
    pub fn new() -> Self {
        Self {
            senders: Mutex::new(Vec::new()),
        }
    }

    /// Creates a new receiver and adds the corresponding sender to the sender
    /// list.
    pub fn create_receiver(&self) -> async_channel::Receiver<T> {
        let mut senders = lock(&self.senders);
        senders.retain(|sender| sender.receiver_count() > 0);
        let (tx, rx) = async_channel::unbounded();
        senders.push(tx);
        rx
    }

    /// Emits an event to all senders. Senders whose receivers were dropped are
    /// removed from the list.
    ///
    /// `try_send` rather than a blocking send: every channel here is
    /// [`async_channel::unbounded`], so the only way a send fails is a closed
    /// receiver — which is precisely the pruning condition. Blocking would be
    /// the same call with a deadlock hazard attached, since this runs while
    /// holding [`Self::senders`].
    pub fn emit(&self, event: T) {
        let mut senders = lock(&self.senders);
        senders
            .retain(|sender| sender.receiver_count() > 0 && sender.try_send(event.clone()).is_ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_receiver_prunes_abandoned_senders_without_waiting_for_emit() {
        let emitter = EventEmitter::<u8>::new();

        let rx = emitter.create_receiver();
        drop(rx);

        let _rx = emitter.create_receiver();

        assert_eq!(emitter.senders.lock().unwrap().len(), 1);
    }

    #[test]
    fn emit_prunes_abandoned_senders() {
        let emitter = EventEmitter::<u8>::new();

        let live = emitter.create_receiver();
        let dropped = emitter.create_receiver();
        drop(dropped);

        emitter.emit(7);

        assert_eq!(live.recv_blocking().unwrap(), 7);
        assert_eq!(emitter.senders.lock().unwrap().len(), 1);
    }
}
