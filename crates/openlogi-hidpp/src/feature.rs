//! Specific device feature implementations.

use std::{any::Any, sync::Arc};

use crate::{
    channel::{
        AbandonedReply, HidppChannel, HidppMessage, LONG_REPORT_LENGTH, MessageListenerGuard,
        RequestSwIds,
    },
    emitter::EventEmitter,
    nibble::U4,
    protocol::v20::{self, Hidpp20Error},
};

pub mod adc_measurement;
pub mod adjustable_dpi;
pub mod backlight;
pub mod battery_status;
pub mod battery_voltage;
pub mod brightness_control;
pub mod change_host;
pub mod color_led_effects;
pub mod crown;
pub mod device_friendly_name;
pub mod device_information;
pub mod device_type_and_name;
pub mod disable_keys;
pub mod disable_keys_by_usage;
pub mod dual_platform;
pub mod equalizer;
pub mod extended_dpi;
pub mod extended_report_rate;
pub mod feature_set;
pub mod fn_inversion;
pub mod force_sensing_button;
pub mod gestures2;
pub mod haptic_feedback;
pub mod hires_wheel;
pub mod hosts_info;
pub mod illumination;
pub mod led_control;
pub mod lock_key_state;
pub mod mode_status;
pub mod mouse_button_spy;
pub mod mouse_pointer;
pub mod multi_platform;
pub mod onboard_profiles;
pub mod per_key_lighting;
pub mod persistent_remappable_action;
pub mod pointer_motion_scaling;
pub mod registry;
pub mod report_rate;
pub mod reprog_controls;
pub mod rgb_effects;
pub mod root;
pub mod sidetone;
pub mod smartshift;
pub mod smartshift_enhanced;
pub mod solar_dashboard;
pub mod thumbwheel;
pub mod touch_mouse_raw;
pub mod touchpad_raw_xy;
pub mod unified_battery;
pub mod vertical_scrolling;
pub mod wireless_device_status;

/// Represents a concrete implementation of a HID++2.0 device feature.
pub trait Feature: Any + Send + Sync {}

/// Represents a [`Feature`] that can be instantiated automatically.
pub trait CreatableFeature: Feature {
    /// The protocol ID of the implemented feature.
    const ID: u16;

    /// The version of the feature the implementation starts to support.
    const STARTING_VERSION: u8;

    /// Creates a new instance of the feature implementation.
    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self;
}

/// Represents a [`Feature`] that emits events of type `T`.
pub trait EmittingFeature<T>: Feature {
    /// Creates a receiver that is being notified whenever a new event of type
    /// `T` is emitted by the feature.
    fn listen(&self) -> async_channel::Receiver<T>;
}

/// Decodes a feature's event payload from its HID++2.0 event sub-id.
///
/// Implemented by a feature's event enum. [`EventSource::attach`] calls
/// [`Self::decode_message`] for every unsolicited broadcast addressed to the
/// feature, and [`Self::decode_foreign_response`] for every response
/// addressed to it that answers another client's request. These are the only
/// parts of a feature's event handling that have to vary — [`EventSource`]
/// owns the listener registration, the [`inbound_report`] filtering, and the
/// emitting. Returning `None` drops the report exactly like the hand-rolled
/// per-feature listeners this replaced did on an unrecognised sub-id or an
/// unparsable payload.
pub(crate) trait DecodeEvent: Clone + Send + Sync + 'static {
    /// Decodes `payload` for event sub-id `sub_id`, or returns `None` to drop
    /// the report.
    fn decode(sub_id: u8, payload: &[u8; LONG_REPORT_LENGTH - 4]) -> Option<Self>
    where
        Self: Sized;

    /// The fewest payload bytes event `sub_id` must carry on the wire. The
    /// default is no minimum.
    ///
    /// [`Self::decode`] sees a zero-extended payload, so it cannot tell a
    /// short report's missing fields from zeroes the device sent.
    fn min_payload_len(_sub_id: u8) -> usize {
        0
    }

    /// Decodes a received event `msg`. Drops a report whose wire payload is
    /// shorter than [`Self::min_payload_len`] before [`Self::decode`] sees
    /// it.
    fn decode_message(msg: &v20::Message) -> Option<Self>
    where
        Self: Sized,
    {
        let sub_id = msg.header().function_id.to_lo();
        if msg.payload_len() < Self::min_payload_len(sub_id) {
            return None;
        }
        Self::decode(sub_id, &msg.extend_payload())
    }

    /// Decodes `response`, a response that answers a request another client
    /// sent, or returns `None` to drop it.
    ///
    /// "Another client" is any sender whose requests carry a software id this
    /// channel never sends (see [`RequestSwIds`]). Its response tells this
    /// channel that the client changed something. The default drops every
    /// such response; a feature decodes one only when its owner must learn of
    /// that change.
    fn decode_foreign_response(_response: &v20::Message) -> Option<Self>
    where
        Self: Sized,
    {
        None
    }
}

/// Owns the [`EventEmitter`] and message listener backing an
/// [`EmittingFeature`] implementation.
///
/// Every emitting feature used to hand-roll the same listener ceremony in its
/// `CreatableFeature::new`: build an `Arc<EventEmitter<E>>`, register a
/// guarded message listener, filter the raw report through [`inbound_report`],
/// and emit whatever the feature's own decode step produced. A feature now
/// holds one `EventSource<E>` field, builds it with [`Self::attach`], and
/// implements [`EmittingFeature`] with a one-line [`Self::listen`] delegate.
pub(crate) struct EventSource<E: DecodeEvent> {
    emitter: Arc<EventEmitter<E>>,

    /// Removes the message listener when the source is dropped.
    _listener: MessageListenerGuard,
}

impl<E: DecodeEvent> EventSource<E> {
    /// Registers a listener on `chan` that decodes the reports addressed to
    /// `(device_index, feature_index)` — broadcasts via
    /// [`DecodeEvent::decode_message`] and other clients' responses via
    /// [`DecodeEvent::decode_foreign_response`] — and emits them to receivers
    /// created by [`Self::listen`].
    pub(crate) fn attach(chan: &Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        let emitter = Arc::new(EventEmitter::new());
        // Read once. Only `set_sw_id_policy` changes the policy, and it needs
        // `&mut HidppChannel`: a feature that decodes foreign responses keeps
        // `chan` shared in its endpoint for as long as this listener lives.
        let own = chan.request_sw_ids();

        let listener = chan.add_msg_listener_guarded({
            let emitter = Arc::clone(&emitter);

            move |raw, matched| {
                let event = match inbound_report(raw, matched, device_index, feature_index, own) {
                    Some(InboundReport::Broadcast(msg)) => E::decode_message(&msg),
                    Some(InboundReport::ForeignResponse(response)) => {
                        E::decode_foreign_response(&response)
                    }
                    None => None,
                };
                if let Some(event) = event {
                    emitter.emit(event);
                }
            }
        });

        Self {
            emitter,
            _listener: listener,
        }
    }

    /// Creates a receiver notified of every event this source decodes.
    pub(crate) fn listen(&self) -> async_channel::Receiver<E> {
        self.emitter.create_receiver()
    }
}

/// A feature's addressable `(device, feature)` endpoint on a channel.
///
/// Embedding this in a feature replaces the three loose `chan` / `device_index`
/// / `feature_index` fields every implementation used to carry, and centralises
/// the HID++2.0 request framing that was otherwise hand-written at every call
/// site.
#[derive(Clone)]
pub(crate) struct FeatureEndpoint {
    /// The underlying HID++ channel.
    chan: Arc<HidppChannel>,

    /// The index of the device the feature belongs to.
    device_index: u8,

    /// The index of the feature in the device's feature table.
    feature_index: u8,
}

impl FeatureEndpoint {
    /// Binds an endpoint to `feature_index` on `device_index` of `chan`.
    pub(crate) fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            chan,
            device_index,
            feature_index,
        }
    }

    /// The request header addressing `function` on this endpoint, stamped with
    /// the channel's next software id.
    ///
    /// `function` is a HID++2.0 function id, which is 4-bit; only the low nibble
    /// is sent. The assert keeps a stray out-of-range id from silently routing
    /// to a different function in debug builds.
    fn header(&self, function: u8) -> v20::MessageHeader {
        debug_assert!(
            function < 16,
            "HID++2.0 function id {function} exceeds 4 bits"
        );
        v20::MessageHeader {
            device_index: self.device_index,
            feature_index: self.feature_index,
            function_id: U4::from_lo(function),
            software_id: self.chan.get_sw_id(),
        }
    }

    /// Calls `function` with a 3-byte short-report payload and waits for the
    /// matching response.
    pub(crate) async fn call(
        &self,
        function: u8,
        args: [u8; 3],
    ) -> Result<v20::Message, Hidpp20Error> {
        self.call_with(function, args, AbandonedReply::Quarantine)
            .await
    }

    /// Short-report call that drains its native write before the response
    /// deadline. The owner must drive it to completion after cancellation.
    /// Unlike RGB ownership commands, it does not require a payload echo.
    pub(crate) async fn call_write_through(
        &self,
        function: u8,
        args: [u8; 3],
    ) -> Result<v20::Message, Hidpp20Error> {
        self.chan
            .send_v20_write_through(v20::Message::Short(self.header(function), args), |_| true)
            .await
    }

    /// [`Self::call`], choosing what to do about a reply still owed to an
    /// abandoned call with the same header. Only a function that reads
    /// state no write can change should pass
    /// [`AbandonedReply::AdoptIdentical`].
    pub(crate) async fn call_with(
        &self,
        function: u8,
        args: [u8; 3],
        abandoned: AbandonedReply,
    ) -> Result<v20::Message, Hidpp20Error> {
        self.chan
            .send_v20_with(v20::Message::Short(self.header(function), args), abandoned)
            .await
    }

    /// Calls `function` with a 16-byte long-report payload and waits for the
    /// matching response.
    pub(crate) async fn call_long(
        &self,
        function: u8,
        args: [u8; 16],
    ) -> Result<v20::Message, Hidpp20Error> {
        self.chan
            .send_v20(v20::Message::Long(self.header(function), args))
            .await
    }

    /// Calls a function that echoes all three request bytes, waiting through
    /// the native write before applying the response deadline. Differing
    /// payloads (e.g. a late ownership claim during rollback) do not match.
    ///
    /// The owner must drive this future to completion; see
    /// [`HidppChannel::send_v20_write_through`].
    pub(crate) async fn call_echoed_write_through(
        &self,
        function: u8,
        args: [u8; 3],
    ) -> Result<v20::Message, Hidpp20Error> {
        self.chan
            .send_v20_write_through(
                v20::Message::Short(self.header(function), args),
                move |response| response.extend_payload()[..3] == args,
            )
            .await
    }

    /// Calls `function` with a 16-byte long-report payload, waiting through the
    /// native write before applying the response deadline.
    ///
    /// The owner must drive this future to completion; see
    /// [`HidppChannel::send_v20_write_through`].
    pub(crate) async fn call_long_write_through(
        &self,
        function: u8,
        args: [u8; 16],
    ) -> Result<v20::Message, Hidpp20Error> {
        self.chan
            .send_v20_write_through(v20::Message::Long(self.header(function), args), |_| true)
            .await
    }

    /// Sends `function` with a 3-byte short-report payload without waiting for a
    /// response.
    ///
    /// For functions the device answers normally use [`Self::call`]; this is for
    /// the rare function whose side effect (e.g. a host switch that resets the
    /// device) prevents a response from ever arriving.
    pub(crate) async fn notify(&self, function: u8, args: [u8; 3]) -> Result<(), Hidpp20Error> {
        self.chan
            .send_and_forget(v20::Message::Short(self.header(function), args).into())
            .await?;
        Ok(())
    }
}

/// A report that a feature's event listener may decode, by who caused it.
///
/// Both variants carry the whole message, so the caller still knows how many
/// payload bytes it carried on the wire.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InboundReport {
    /// A device notification: software id `0`. Its function id is the event's
    /// sub-id.
    Broadcast(v20::Message),
    /// A response to a request that another client sent: a nonzero software
    /// id that the channel's own requests never carry.
    ForeignResponse(v20::Message),
}

/// Shared prelude for a feature's event listener.
///
/// Drops reports already matched to an outgoing request, parses the raw report
/// as a HID++2.0 message, and keeps only the reports addressed to this
/// `(device_index, feature_index)` that no request of this channel caused:
/// broadcasts with a zero software id, and responses whose software id is not
/// in `own`. A response with an id in `own` can answer one of this channel's
/// own requests even when nothing matched it — late, or abandoned — so it is
/// dropped. Sub-id dispatch is left to the caller, so a multi-event feature
/// filters its sub-ids explicitly rather than folding the check into the
/// header guard.
pub(crate) fn inbound_report(
    raw: HidppMessage,
    matched: bool,
    device_index: u8,
    feature_index: u8,
    own: RequestSwIds,
) -> Option<InboundReport> {
    if matched {
        return None;
    }

    let msg = v20::Message::from(raw);
    let header = msg.header();
    if header.device_index != device_index || header.feature_index != feature_index {
        return None;
    }
    if header.software_id.to_lo() == 0 {
        Some(InboundReport::Broadcast(msg))
    } else if own.contains(header.software_id) {
        None
    } else {
        Some(InboundReport::ForeignResponse(msg))
    }
}

bitflags::bitflags! {
    /// A bitfield describing some properties of a feature.
    ///
    /// Documentation is taken from <https://drive.google.com/file/d/1ULmw9uJL8b8iwwUo5xjSS9F5Zvno-86y/view>.
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct FeatureType: u8 {
        /// An obsolete feature is a feature that has been replaced by a newer
        /// one, but is advertised in order for older SWs to still be able to
        /// support the feature (in case the old SW does not know yet the newer
        /// one).
        const OBSOLETE = 1 << 7;

        /// A SW hidden feature is a feature that should not be
        /// known/managed/used by end user configuration SW. The host should
        /// ignore this type of features.
        const HIDDEN = 1 << 6;

        /// A hidden feature that has been disabled for user software. Used for
        /// internal testing and manufacturing.
        const ENGINEERING = 1 << 5;

        /// A manufacturing feature that can be permanently deactivated. It is
        /// usually also hidden and engineering.
        ///
        /// This flag was added in feature version 2 and is never set by older
        /// versions.
        const MANUFACTURING_DEACTIVATABLE = 1 << 4;

        /// A compliance feature that can be permanently deactivated. It is
        /// usually also hidden and engineering.
        ///
        /// This flag was added in feature version 2 and is never set by older
        /// versions.
        const COMPLIANCE_DEACTIVATABLE = 1 << 3;
    }
}

impl FeatureType {
    /// Whether end-user configuration software may manage a feature with these flags.
    ///
    /// The HID++ feature-type contract requires host software to ignore hidden
    /// and engineering features. Other flags, including obsolete, do not make a
    /// feature unavailable to existing user software.
    #[must_use]
    pub const fn is_end_user_configurable(self) -> bool {
        !self.contains(Self::HIDDEN) && !self.contains(Self::ENGINEERING)
    }
}

#[cfg(test)]
mod tests {
    use super::{DecodeEvent, FeatureType, InboundReport, inbound_report};
    use crate::{
        channel::{HidppMessage, RequestSwIds, SwIdPolicy},
        nibble::U4,
        protocol::v20::{Message, MessageHeader},
    };

    /// The default policy's set: id `1` only.
    fn own() -> RequestSwIds {
        SwIdPolicy::default().request_sw_ids()
    }

    #[test]
    fn hidden_and_engineering_features_are_not_end_user_configurable() {
        for typ in [FeatureType::HIDDEN, FeatureType::ENGINEERING] {
            assert!(!typ.is_end_user_configurable(), "{typ:?}");
        }
        assert!(FeatureType::OBSOLETE.is_end_user_configurable());
    }

    fn header(device_index: u8, feature_index: u8, function: u8, software: u8) -> MessageHeader {
        MessageHeader {
            device_index,
            feature_index,
            function_id: U4::from_lo(function),
            software_id: U4::from_lo(software),
        }
    }

    /// Builds a HID++2.0 long report with the given header fields and a
    /// recognisable payload.
    fn message(device_index: u8, feature_index: u8, function: u8, software: u8) -> Message {
        Message::Long(
            header(device_index, feature_index, function, software),
            [0xab; 16],
        )
    }

    /// [`message`] as the raw report a listener receives.
    fn report(device_index: u8, feature_index: u8, function: u8, software: u8) -> HidppMessage {
        message(device_index, feature_index, function, software).into()
    }

    #[test]
    fn accepts_matching_broadcast_and_returns_sub_id() {
        let Some(InboundReport::Broadcast(msg)) =
            inbound_report(report(2, 5, 1, 0), false, 2, 5, own())
        else {
            panic!("broadcast should pass");
        };
        assert_eq!(msg.header().function_id.to_lo(), 1);
        assert_eq!(msg.extend_payload(), [0xab; 16]);
    }

    #[test]
    fn rejects_request_matched_report() {
        // A report already matched to an outgoing request is a response, not an
        // event, whatever software id it carries.
        assert!(inbound_report(report(2, 5, 0, 0), true, 2, 5, own()).is_none());
        assert!(inbound_report(report(2, 5, 3, 9), true, 2, 5, own()).is_none());
    }

    #[test]
    fn rejects_other_device_or_feature() {
        for software in [0, 9] {
            assert!(inbound_report(report(9, 5, 3, software), false, 2, 5, own()).is_none());
            assert!(inbound_report(report(2, 9, 3, software), false, 2, 5, own()).is_none());
        }
    }

    #[test]
    fn gates_on_software_id_only_not_sub_id() {
        // Only the software id tells a broadcast from a response: a nonzero
        // function id is a valid event sub-id the caller dispatches on and
        // must still pass as a broadcast. This is the invariant the old per-feature
        // `nibble::combine(software_id, function_id) != 0` guard got right only
        // by accident (those features happened to emit a single sub-id 0 event).
        assert!(matches!(
            inbound_report(report(2, 5, 7, 0), false, 2, 5, own()),
            Some(InboundReport::Broadcast(..))
        ));
    }

    #[test]
    fn drops_an_unmatched_response_this_channel_may_have_asked_for() {
        // A late or abandoned reply to this channel's own request matches
        // nothing, but it reports no change made by anyone else.
        assert!(inbound_report(report(2, 5, 3, 1), false, 2, 5, own()).is_none());
    }

    #[test]
    fn keeps_another_clients_response_whole() {
        for software in [2, 9, 15] {
            assert_eq!(
                inbound_report(report(2, 5, 3, software), false, 2, 5, own()),
                Some(InboundReport::ForeignResponse(message(2, 5, 3, software))),
                "software id {software}"
            );
        }
    }

    /// An event that keeps the sub-id and the first payload byte, and needs
    /// four payload bytes for sub-id 1 only.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Probe(u8, u8);

    impl DecodeEvent for Probe {
        fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
            Some(Self(sub_id, payload[0]))
        }

        fn min_payload_len(sub_id: u8) -> usize {
            if sub_id == 1 { 4 } else { 0 }
        }
    }

    #[test]
    fn decode_message_drops_a_wire_payload_below_the_minimum() {
        // A short report carries 3 payload bytes: below sub-id 1's minimum,
        // enough for sub-id 0, which has none.
        let short = |function| Message::Short(header(2, 5, function, 0), [7, 8, 9]);
        assert_eq!(Probe::decode_message(&short(1)), None);
        assert_eq!(Probe::decode_message(&short(0)), Some(Probe(0, 7)));
        // The same sub-id 1 event in a long report decodes.
        let long = Message::Long(header(2, 5, 1, 0), [7; 16]);
        assert_eq!(Probe::decode_message(&long), Some(Probe(1, 7)));
    }
}
