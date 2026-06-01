use std::sync::Mutex;

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
        let mut senders = self.senders.lock().unwrap();
        let (tx, rx) = async_channel::unbounded();
        senders.push(tx);
        rx
    }

    /// Emits an event to all senders. Senders whose receivers were dropped are
    /// removed from the list.
    pub fn emit(&self, event: T) {
        let mut senders = self.senders.lock().unwrap();
        senders.retain(|sender| sender.send_blocking(event.clone()).is_ok());
    }
}
