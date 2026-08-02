// Copyright 2022 Google LLC
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

//! Provides a hook that runs after request execution.

use crate::{context, server::Serve, ServerError};
use futures::prelude::*;

/// A hook that runs after request execution.
#[allow(async_fn_in_trait)]
pub trait AfterRequest<Resp> {
    /// The function that is called after request execution.
    ///
    /// The hook can modify the request context and the response.
    async fn after(&mut self, ctx: &mut context::Context, resp: &mut Result<Resp, ServerError>);
}

impl<F, Fut, Resp> AfterRequest<Resp> for F
where
    F: FnMut(&mut context::Context, &mut Result<Resp, ServerError>) -> Fut,
    Fut: Future<Output = ()>,
{
    async fn after(&mut self, ctx: &mut context::Context, resp: &mut Result<Resp, ServerError>) {
        self(ctx, resp).await
    }
}

/// A Service function that runs a hook after request execution.
pub struct ServeThenHook<Serv, Hook> {
    serve: Serv,
    hook: Hook,
}

impl<Serv, Hook> ServeThenHook<Serv, Hook> {
    pub(crate) fn new(serve: Serv, hook: Hook) -> Self {
        Self { serve, hook }
    }
}

impl<Serv: Clone, Hook: Clone> Clone for ServeThenHook<Serv, Hook> {
    fn clone(&self) -> Self {
        Self {
            serve: self.serve.clone(),
            hook: self.hook.clone(),
        }
    }
}

impl<Serv, Hook> Serve for ServeThenHook<Serv, Hook>
where
    Serv: Serve,
    Hook: AfterRequest<Serv::Resp>,
{
    type Req = Serv::Req;
    type Resp = Serv::Resp;

    async fn serve(
        self,
        mut ctx: context::Context,
        req: Serv::Req,
    ) -> Result<Serv::Resp, ServerError> {
        let ServeThenHook {
            serve, mut hook, ..
        } = self;
        let mut resp = serve.serve(ctx, req).await;
        hook.after(&mut ctx, &mut resp).await;
        resp
    }
}
