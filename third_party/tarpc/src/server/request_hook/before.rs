// Copyright 2022 Google LLC
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

//! Provides a hook that runs before request execution.

use crate::{context, server::Serve, ServerError};
use futures::prelude::*;

/// A hook that runs before request execution.
#[allow(async_fn_in_trait)]
pub trait BeforeRequest<Req> {
    /// The function that is called before request execution.
    ///
    /// If this function returns an error, the request will not be executed and the error will be
    /// returned instead.
    ///
    /// This function can also modify the request context. This could be used, for example, to
    /// enforce a maximum deadline on all requests.
    async fn before(&mut self, ctx: &mut context::Context, req: &Req) -> Result<(), ServerError>;
}

/// A list of hooks that run in order before request execution.
pub trait BeforeRequestList<Req>: BeforeRequest<Req> {
    /// The hook returned by `BeforeRequestList::then`.
    type Then<Next>: BeforeRequest<Req>
    where
        Next: BeforeRequest<Req>;

    /// Returns a hook that, when run, runs two hooks, first `self` and then `next`.
    fn then<Next: BeforeRequest<Req>>(self, next: Next) -> Self::Then<Next>;

    /// Same as `then`, but helps the compiler with type inference when Next is a closure.
    fn then_fn<
        Next: FnMut(&mut context::Context, &Req) -> Fut,
        Fut: Future<Output = Result<(), ServerError>>,
    >(
        self,
        next: Next,
    ) -> Self::Then<Next>
    where
        Self: Sized,
    {
        self.then(next)
    }

    /// The service fn returned by `BeforeRequestList::serving`.
    type Serve<S: Serve<Req = Req>>: Serve<Req = Req>;

    /// Runs the list of request hooks before execution of the given serve fn.
    /// This is equivalent to `serve.before(before_request_chain)` but may be syntactically nicer.
    fn serving<S: Serve<Req = Req>>(self, serve: S) -> Self::Serve<S>;
}

impl<F, Fut, Req> BeforeRequest<Req> for F
where
    F: FnMut(&mut context::Context, &Req) -> Fut,
    Fut: Future<Output = Result<(), ServerError>>,
{
    async fn before(&mut self, ctx: &mut context::Context, req: &Req) -> Result<(), ServerError> {
        self(ctx, req).await
    }
}

/// A Service function that runs a hook before request execution.
#[derive(Clone)]
pub struct HookThenServe<Serv, Hook> {
    serve: Serv,
    hook: Hook,
}

impl<Serv, Hook> HookThenServe<Serv, Hook> {
    pub(crate) fn new(serve: Serv, hook: Hook) -> Self {
        Self { serve, hook }
    }
}

impl<Serv, Hook> Serve for HookThenServe<Serv, Hook>
where
    Serv: Serve,
    Hook: BeforeRequest<Serv::Req>,
{
    type Req = Serv::Req;
    type Resp = Serv::Resp;

    async fn serve(
        self,
        mut ctx: context::Context,
        req: Self::Req,
    ) -> Result<Serv::Resp, ServerError> {
        let HookThenServe {
            serve, mut hook, ..
        } = self;
        hook.before(&mut ctx, &req).await?;
        serve.serve(ctx, req).await
    }
}

/// Returns a request hook builder that runs a series of hooks before request execution.
///
/// Example
///
/// ```rust
/// use futures::{executor::block_on, future};
/// use tarpc::{context, ServerError, server::{Serve, serve, request_hook::{self,
///             BeforeRequest, BeforeRequestList}}};
/// use std::{cell::Cell, io};
///
/// let i = Cell::new(0);
/// let serve = request_hook::before()
///     .then_fn(|_, _| async {
///         assert!(i.get() == 0);
///         i.set(1);
///         Ok(())
///     })
///     .then_fn(|_, _| async {
///         assert!(i.get() == 1);
///         i.set(2);
///         Ok(())
///     })
///     .serving(serve(|_ctx, i| async move { Ok(i + 1) }));
/// let response = serve.clone().serve(context::current(), 1);
/// assert!(block_on(response).is_ok());
/// assert!(i.get() == 2);
/// ```
pub fn before() -> BeforeRequestNil {
    BeforeRequestNil
}

/// A list of hooks that run in order before a request is executed.
#[derive(Clone, Copy)]
pub struct BeforeRequestCons<First, Rest>(First, Rest);

/// A noop hook that runs before a request is executed.
#[derive(Clone, Copy)]
pub struct BeforeRequestNil;

impl<Req, First: BeforeRequest<Req>, Rest: BeforeRequest<Req>> BeforeRequest<Req>
    for BeforeRequestCons<First, Rest>
{
    async fn before(&mut self, ctx: &mut context::Context, req: &Req) -> Result<(), ServerError> {
        let BeforeRequestCons(first, rest) = self;
        first.before(ctx, req).await?;
        rest.before(ctx, req).await?;
        Ok(())
    }
}

impl<Req> BeforeRequest<Req> for BeforeRequestNil {
    async fn before(&mut self, _: &mut context::Context, _: &Req) -> Result<(), ServerError> {
        Ok(())
    }
}

impl<Req, First: BeforeRequest<Req>, Rest: BeforeRequestList<Req>> BeforeRequestList<Req>
    for BeforeRequestCons<First, Rest>
{
    type Then<Next>
        = BeforeRequestCons<First, Rest::Then<Next>>
    where
        Next: BeforeRequest<Req>;

    fn then<Next: BeforeRequest<Req>>(self, next: Next) -> Self::Then<Next> {
        let BeforeRequestCons(first, rest) = self;
        BeforeRequestCons(first, rest.then(next))
    }

    type Serve<S: Serve<Req = Req>> = HookThenServe<S, Self>;

    fn serving<S: Serve<Req = Req>>(self, serve: S) -> Self::Serve<S> {
        HookThenServe::new(serve, self)
    }
}

impl<Req> BeforeRequestList<Req> for BeforeRequestNil {
    type Then<Next>
        = BeforeRequestCons<Next, BeforeRequestNil>
    where
        Next: BeforeRequest<Req>;

    fn then<Next: BeforeRequest<Req>>(self, next: Next) -> Self::Then<Next> {
        BeforeRequestCons(next, BeforeRequestNil)
    }

    type Serve<S: Serve<Req = Req>> = S;

    fn serving<S: Serve<Req = Req>>(self, serve: S) -> S {
        serve
    }
}

#[test]
fn before_request_list() {
    use crate::server::serve;
    use futures::executor::block_on;
    use std::cell::Cell;

    let i = Cell::new(0);
    let serve = before()
        .then_fn(|_, _| async {
            assert!(i.get() == 0);
            i.set(1);
            Ok(())
        })
        .then_fn(|_, _| async {
            assert!(i.get() == 1);
            i.set(2);
            Ok(())
        })
        .serving(serve(|_ctx, i| async move { Ok(i + 1) }));
    let response = serve.clone().serve(context::current(), 1);
    assert!(block_on(response).is_ok());
    assert!(i.get() == 2);
}
