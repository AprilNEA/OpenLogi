//! OpenLogi Flow's peer-protocol core.
//!
//! The normative protocol is [`FLOW-PROTOCOL.md`], and the authoritative
//! protobuf schema is [`proto/flow.v1.proto`]. The envelope layout
//! (`kind: u16 LE`, `flags: u16 LE`, `len: u32 LE`, then payload) and the
//! existing fields and encoding of `Hello` are frozen forever.
//!
//! [`FLOW-PROTOCOL.md`]: ../../../docs/FLOW-PROTOCOL.md
//! [`proto/flow.v1.proto`]: ../proto/flow.v1.proto

pub mod frame;
pub mod identity;
pub mod negotiation;
pub mod pairing;
pub mod sas;

/// Types generated from the authoritative Flow protobuf schema.
// Buffa emits compatibility suppressions and intentionally mechanical code.
// The allow is scoped to generated include expansions; clippy cannot credit an
// `expect` with lints originating there, so fulfilment cannot be tracked.
#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::allow_attributes,
    clippy::allow_attributes_without_reason,
    reason = "buffa-generated code is not maintained by this crate"
)]
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_include.rs"));
}

/// Generated message and enum types for protocol version 1.
pub use proto::openlogi::flow::v1 as generated;
