//! Provides a stub that retries requests based on response contents..

use crate::{
    client::{stub, RpcError},
    context, RequestName,
};
use std::sync::Arc;

impl<Stub, Req, F> stub::Stub for Retry<F, Stub>
where
    Req: RequestName,
    Stub: stub::Stub<Req = Arc<Req>>,
    F: Fn(&Result<Stub::Resp, RpcError>, u32) -> bool,
{
    type Req = Req;
    type Resp = Stub::Resp;

    async fn call(
        &self,
        ctx: context::Context,
        request: Self::Req,
    ) -> Result<Stub::Resp, RpcError> {
        let request = Arc::new(request);
        for i in 1.. {
            let result = self.stub.call(ctx, Arc::clone(&request)).await;
            if (self.should_retry)(&result, i) {
                tracing::trace!("Retrying on attempt {i}");
                continue;
            }
            return result;
        }
        unreachable!("Wow, that was a lot of attempts!");
    }
}

/// A Stub that retries requests based on response contents.
/// Note: to use this stub with Serde serialization, the "rc" feature of Serde needs to be enabled.
#[derive(Clone, Debug)]
pub struct Retry<F, Stub> {
    should_retry: F,
    stub: Stub,
}

impl<Stub, Req, F> Retry<F, Stub>
where
    Stub: stub::Stub<Req = Arc<Req>>,
    F: Fn(&Result<Stub::Resp, RpcError>, u32) -> bool,
{
    /// Creates a new Retry stub that delegates calls to the underlying `stub`.
    pub fn new(stub: Stub, should_retry: F) -> Self {
        Self { stub, should_retry }
    }
}
