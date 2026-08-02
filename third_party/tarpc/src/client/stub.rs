//! Provides a Stub trait, implemented by types that can call remote services.

use crate::{
    client::{Channel, RpcError},
    context,
    server::Serve,
    RequestName,
};

pub mod load_balance;
pub mod retry;

#[cfg(test)]
mod mock;

/// A connection to a remote service.
/// Calls the service with requests of type `Req` and receives responses of type `Resp`.
#[allow(async_fn_in_trait)]
pub trait Stub {
    /// The service request type.
    type Req: RequestName;

    /// The service response type.
    type Resp;

    /// Calls a remote service.
    async fn call(&self, ctx: context::Context, request: Self::Req)
        -> Result<Self::Resp, RpcError>;
}

impl<Req, Resp> Stub for Channel<Req, Resp>
where
    Req: RequestName,
{
    type Req = Req;
    type Resp = Resp;

    async fn call(&self, ctx: context::Context, request: Req) -> Result<Self::Resp, RpcError> {
        Self::call(self, ctx, request).await
    }
}

impl<S> Stub for S
where
    S: Serve + Clone,
{
    type Req = S::Req;
    type Resp = S::Resp;
    async fn call(&self, ctx: context::Context, req: Self::Req) -> Result<Self::Resp, RpcError> {
        self.clone().serve(ctx, req).await.map_err(RpcError::Server)
    }
}
