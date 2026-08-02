// Copyright 2018 Google LLC
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

#![deny(missing_docs, missing_debug_implementations)]

//! Provides building blocks for tracing distributed programs.
//!
//! A trace is logically a tree of causally-related events called spans. Traces are tracked via a
//! [context](Context) that identifies the current trace, span, and parent of the current span.  In
//! distributed systems, a context can be sent from client to server to connect events occurring on
//! either side.
//!
//! This crate's design is based on [opencensus
//! tracing](https://opencensus.io/core-concepts/tracing/).

use opentelemetry::trace::TraceContextExt;
use rand::Rng;
use std::{
    convert::TryFrom,
    fmt::{self, Formatter},
    num::{NonZeroU128, NonZeroU64},
};
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// A context for tracing the execution of processes, distributed or otherwise.
///
/// Consists of a span identifying an event, an optional parent span identifying a causal event
/// that triggered the current span, and a trace with which all related spans are associated.
#[derive(Debug, Default, PartialEq, Eq, Hash, Clone, Copy)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
pub struct Context {
    /// An identifier of the trace associated with the current context. A trace ID is typically
    /// created at a root span and passed along through all causal events.
    pub trace_id: TraceId,
    /// An identifier of the current span. In typical RPC usage, a span is created by a client
    /// before making an RPC, and the span ID is sent to the server. The server is free to create
    /// its own spans, for which it sets the client's span as the parent span.
    pub span_id: SpanId,
    /// Indicates whether a sampler has already decided whether or not to sample the trace
    /// associated with the Context. If `sampling_decision` is None, then a decision has not yet
    /// been made. Downstream samplers do not need to abide by "no sample" decisions--for example,
    /// an upstream client may choose to never sample, which may not make sense for the client's
    /// dependencies. On the other hand, if an upstream process has chosen to sample this trace,
    /// then the downstream samplers are expected to respect that decision and also sample the
    /// trace. Otherwise, the full trace would not be able to be reconstructed.
    pub sampling_decision: SamplingDecision,
}

/// A 128-bit UUID identifying a trace. All spans caused by the same originating span share the
/// same trace ID.
#[derive(Default, PartialEq, Eq, Hash, Clone, Copy)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
pub struct TraceId(#[cfg_attr(feature = "serde1", serde(with = "u128_serde"))] u128);

/// A 64-bit identifier of a span within a trace. The identifier is unique within the span's trace.
#[derive(Default, PartialEq, Eq, Hash, Clone, Copy)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
pub struct SpanId(u64);

/// Indicates whether a sampler has decided whether or not to sample the trace associated with the
/// Context. Downstream samplers do not need to abide by "no sample" decisions--for example, an
/// upstream client may choose to never sample, which may not make sense for the client's
/// dependencies. On the other hand, if an upstream process has chosen to sample this trace, then
/// the downstream samplers are expected to respect that decision and also sample the trace.
/// Otherwise, the full trace would not be able to be reconstructed reliably.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum SamplingDecision {
    /// The associated span was sampled by its creating process. Child spans must also be sampled.
    Sampled,
    /// The associated span was not sampled by its creating process.
    Unsampled,
}

impl Context {
    /// Constructs a new context with the trace ID and sampling decision inherited from the parent.
    pub(crate) fn new_child(&self) -> Self {
        Self {
            trace_id: self.trace_id,
            span_id: SpanId::random(&mut rand::thread_rng()),
            sampling_decision: self.sampling_decision,
        }
    }
}

impl TraceId {
    /// Returns a random trace ID that can be assumed to be globally unique if `rng` generates
    /// actually-random numbers.
    pub fn random<R: Rng>(rng: &mut R) -> Self {
        TraceId(rng.gen::<NonZeroU128>().get())
    }

    /// Returns true iff the trace ID is 0.
    pub fn is_none(&self) -> bool {
        self.0 == 0
    }
}

impl SpanId {
    /// Returns a random span ID that can be assumed to be unique within a single trace.
    pub fn random<R: Rng>(rng: &mut R) -> Self {
        SpanId(rng.gen::<NonZeroU64>().get())
    }

    /// Returns true iff the span ID is 0.
    pub fn is_none(&self) -> bool {
        self.0 == 0
    }
}

impl From<TraceId> for u128 {
    fn from(trace_id: TraceId) -> Self {
        trace_id.0
    }
}

impl From<u128> for TraceId {
    fn from(trace_id: u128) -> Self {
        Self(trace_id)
    }
}

impl From<SpanId> for u64 {
    fn from(span_id: SpanId) -> Self {
        span_id.0
    }
}

impl From<u64> for SpanId {
    fn from(span_id: u64) -> Self {
        Self(span_id)
    }
}

impl From<opentelemetry::trace::TraceId> for TraceId {
    fn from(trace_id: opentelemetry::trace::TraceId) -> Self {
        Self::from(u128::from_be_bytes(trace_id.to_bytes()))
    }
}

impl From<TraceId> for opentelemetry::trace::TraceId {
    fn from(trace_id: TraceId) -> Self {
        Self::from_bytes(u128::from(trace_id).to_be_bytes())
    }
}

impl From<opentelemetry::trace::SpanId> for SpanId {
    fn from(span_id: opentelemetry::trace::SpanId) -> Self {
        Self::from(u64::from_be_bytes(span_id.to_bytes()))
    }
}

impl From<SpanId> for opentelemetry::trace::SpanId {
    fn from(span_id: SpanId) -> Self {
        Self::from_bytes(u64::from(span_id).to_be_bytes())
    }
}

impl TryFrom<&tracing::Span> for Context {
    type Error = NoActiveSpan;

    fn try_from(span: &tracing::Span) -> Result<Self, NoActiveSpan> {
        let context = span.context();
        if context.has_active_span() {
            Ok(Self::from(context.span()))
        } else {
            Err(NoActiveSpan)
        }
    }
}

impl From<opentelemetry::trace::SpanRef<'_>> for Context {
    fn from(span: opentelemetry::trace::SpanRef<'_>) -> Self {
        let otel_ctx = span.span_context();
        Self {
            trace_id: TraceId::from(otel_ctx.trace_id()),
            span_id: SpanId::from(otel_ctx.span_id()),
            sampling_decision: SamplingDecision::from(otel_ctx),
        }
    }
}

impl From<SamplingDecision> for opentelemetry::trace::TraceFlags {
    fn from(decision: SamplingDecision) -> Self {
        match decision {
            SamplingDecision::Sampled => opentelemetry::trace::TraceFlags::SAMPLED,
            SamplingDecision::Unsampled => opentelemetry::trace::TraceFlags::default(),
        }
    }
}

impl From<&opentelemetry::trace::SpanContext> for SamplingDecision {
    fn from(context: &opentelemetry::trace::SpanContext) -> Self {
        if context.is_sampled() {
            SamplingDecision::Sampled
        } else {
            SamplingDecision::Unsampled
        }
    }
}

impl Default for SamplingDecision {
    fn default() -> Self {
        Self::Unsampled
    }
}

/// Returned when a [`Context`] cannot be constructed from a [`Span`](tracing::Span).
#[derive(Debug)]
pub struct NoActiveSpan;

impl fmt::Display for TraceId {
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        write!(f, "{:02x}", self.0)?;
        Ok(())
    }
}

impl fmt::Debug for TraceId {
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        write!(f, "{:02x}", self.0)?;
        Ok(())
    }
}

impl fmt::Display for SpanId {
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        write!(f, "{:02x}", self.0)?;
        Ok(())
    }
}

impl fmt::Debug for SpanId {
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        write!(f, "{:02x}", self.0)?;
        Ok(())
    }
}

#[cfg(feature = "serde1")]
mod u128_serde {
    pub fn serialize<S>(u: &u128, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serde::Serialize::serialize(&u.to_le_bytes(), serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<u128, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(u128::from_le_bytes(serde::Deserialize::deserialize(
            deserializer,
        )?))
    }
}
