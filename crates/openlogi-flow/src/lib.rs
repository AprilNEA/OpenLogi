//! OpenLogi Flow's peer-protocol core.
//!
//! The normative protocol is [`FLOW-PROTOCOL.md`], and the authoritative
//! protobuf schema is [`proto/flow.v1.proto`]. The envelope layout
//! (`kind: u16 LE`, `flags: u16 LE`, `len: u32 LE`, then payload) and the
//! existing fields and encoding of `Hello` are frozen forever.
//!
//! [`FLOW-PROTOCOL.md`]: ../../../docs/FLOW-PROTOCOL.md
//! [`proto/flow.v1.proto`]: ../proto/flow.v1.proto

pub mod discovery;
pub mod frame;
pub mod identity;
pub mod negotiation;
pub mod pairing;
pub mod sas;
pub mod session;
pub mod transport;

/// Types generated from the authoritative Flow protobuf schema.
// Buffa emits compatibility suppressions and intentionally mechanical code.
// The allow is scoped to generated include expansions; clippy cannot credit an
// `expect` with lints originating there, so fulfilment cannot be tracked.
// The lints are named rather than grouped so that the weekly stale-allows
// sweep can judge each one; it cannot judge a lint group.
#[allow(
    clippy::cast_lossless,
    clippy::default_trait_access,
    clippy::elidable_lifetime_names,
    clippy::missing_fields_in_debug,
    clippy::pub_underscore_fields,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::single_match_else,
    clippy::allow_attributes,
    clippy::allow_attributes_without_reason,
    reason = "buffa-generated code is not maintained by this crate"
)]
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_include.rs"));
}

/// Generated message and enum types for protocol version 1.
pub use proto::openlogi::flow::v1 as generated;
