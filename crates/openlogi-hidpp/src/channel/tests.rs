//! Channel tests: the shared fixtures, and one module per area.

use super::mock::{MockRawHidChannel, channel_with_reader};
use super::*;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{
    nibble,
    protocol::v20::{self, ErrorType, Hidpp20Error},
};

mod lease;
mod pause;
mod raw_and_listeners;
mod same_header;
mod send;
mod send_v20;

fn short_msg(marker: u8) -> HidppMessage {
    HidppMessage::Short([0xff, marker, 0x10, marker, marker, marker])
}

/// A request with [`short_msg`]'s header for `marker` but its own payload —
/// a different question that the wire answers under the same header.
fn same_header_msg(marker: u8, payload: u8) -> HidppMessage {
    HidppMessage::Short([0xff, marker, 0x10, payload, payload, payload])
}

fn assert_pending_empty(channel: &HidppChannel) {
    assert_eq!(pending_len(channel), 0);
}

fn pending_len(channel: &HidppChannel) -> usize {
    channel.pending_messages.lock().unwrap().in_flight_count()
}

fn stale_len(channel: &HidppChannel) -> usize {
    channel.pending_messages.lock().unwrap().stale_count()
}

async fn wait_for_event_count(events: &Arc<Mutex<Vec<(HidppMessage, bool)>>>, count: usize) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(1) {
        if events.lock().unwrap().len() >= count {
            return;
        }
        futures_timer::Delay::new(Duration::from_millis(10)).await;
    }

    panic!("timed out waiting for {count} listener events");
}
