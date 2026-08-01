// Copyright 2022 Google LLC
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

//! Provides a hook that runs both before and after request execution.

use super::{after::AfterRequest, before::BeforeRequest};
use crate::{context, server::Serve, RequestName, ServerError};
use std::marker::PhantomData;

/// A Service function that runs a hook both before and after request execution.
pub struct HookThenServeThenHook<Req, Resp, Serv, Hook> {
    serve: Serv,
    hook: Hook,
    fns: PhantomData<(fn(Req), fn(Resp))>,
}

impl<Req, Resp, Serv, Hook> HookThenServeThenHook<Req, Resp, Serv, Hook> {
    pub(crate) fn new(serve: Serv, hook: Hook) -> Self {
        Self {
            serve,
            hook,
            fns: PhantomData,
        }
    }
}

impl<Req, Resp, Serv: Clone, Hook: Clone> Clone for HookThenServeThenHook<Req, Resp, Serv, Hook> {
    fn clone(&self) -> Self {
        Self {
            serve: self.serve.clone(),
            hook: self.hook.clone(),
            fns: PhantomData,
        }
    }
}

impl<Req, Resp, Serv, Hook> Serve for HookThenServeThenHook<Req, Resp, Serv, Hook>
where
    Req: RequestName,
    Serv: Serve<Req = Req, Resp = Resp>,
    Hook: BeforeRequest<Req> + AfterRequest<Resp>,
{
    type Req = Req;
    type Resp = Resp;

    async fn serve(self, mut ctx: context::Context, req: Req) -> Result<Serv::Resp, ServerError> {
        let HookThenServeThenHook {
            serve, mut hook, ..
        } = self;
        hook.before(&mut ctx, &req).await?;
        let mut resp = serve.serve(ctx, req).await;
        hook.after(&mut ctx, &mut resp).await;
        resp
    }
}
