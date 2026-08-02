// Copyright 2022 Google LLC
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

//! Hooks for horizontal functionality that can run either before or after a request is executed.

use crate::server::Serve;

/// A request hook that runs before a request is executed.
mod before;

/// A request hook that runs after a request is completed.
mod after;

/// A request hook that runs both before a request is executed and after it is completed.
mod before_and_after;

pub use {
    after::{AfterRequest, ServeThenHook},
    before::{
        before, BeforeRequest, BeforeRequestCons, BeforeRequestList, BeforeRequestNil,
        HookThenServe,
    },
    before_and_after::HookThenServeThenHook,
};

/// Hooks that run before and/or after serving a request.
pub trait RequestHook: Serve {
    /// Runs a hook before execution of the request.
    ///
    /// If the hook returns an error, the request will not be executed and the error will be
    /// returned instead.
    ///
    /// The hook can also modify the request context. This could be used, for example, to enforce a
    /// maximum deadline on all requests.
    ///
    /// Any type that implements [`BeforeRequest`] can be used as the hook. Types that implement
    /// `FnMut(&mut Context, &RequestType) -> impl Future<Output = Result<(), ServerError>>` can
    /// also be used.
    ///
    /// # Example
    ///
    /// ```rust
    /// use futures::{executor::block_on, future};
    /// use tarpc::{context, ServerError, server::{Serve, request_hook::RequestHook, serve}};
    /// use std::io;
    ///
    /// let serve = serve(|_ctx, i| async move { Ok(i + 1) })
    ///     .before(|_ctx: &mut context::Context, req: &i32| {
    ///         future::ready(
    ///             if *req == 1 {
    ///                 Err(ServerError::new(
    ///                     io::ErrorKind::Other,
    ///                     format!("I don't like {req}")))
    ///             } else {
    ///                 Ok(())
    ///             })
    ///     });
    /// let response = serve.serve(context::current(), 1);
    /// assert!(block_on(response).is_err());
    /// ```
    fn before<Hook>(self, hook: Hook) -> HookThenServe<Self, Hook>
    where
        Hook: BeforeRequest<Self::Req>,
        Self: Sized,
    {
        HookThenServe::new(self, hook)
    }

    /// Runs a hook after completion of a request.
    ///
    /// The hook can modify the request context and the response.
    ///
    /// Any type that implements [`AfterRequest`] can be used as the hook. Types that implement
    /// `FnMut(&mut Context, &mut Result<ResponseType, ServerError>) -> impl Future<Output = ()>`
    /// can also be used.
    ///
    /// # Example
    ///
    /// ```rust
    /// use futures::{executor::block_on, future};
    /// use tarpc::{context, ServerError, server::{Serve, request_hook::RequestHook, serve}};
    /// use std::io;
    ///
    /// let serve = serve(
    ///     |_ctx, i| async move {
    ///         if i == 1 {
    ///             Err(ServerError::new(
    ///                 io::ErrorKind::Other,
    ///                 format!("{i} is the loneliest number")))
    ///         } else {
    ///             Ok(i + 1)
    ///         }
    ///     })
    ///     .after(|_ctx: &mut context::Context, resp: &mut Result<i32, ServerError>| {
    ///         if let Err(e) = resp {
    ///             eprintln!("server error: {e:?}");
    ///         }
    ///         future::ready(())
    ///     });
    ///
    /// let response = serve.serve(context::current(), 1);
    /// assert!(block_on(response).is_err());
    /// ```
    fn after<Hook>(self, hook: Hook) -> ServeThenHook<Self, Hook>
    where
        Hook: AfterRequest<Self::Resp>,
        Self: Sized,
    {
        ServeThenHook::new(self, hook)
    }

    /// Runs a hook before and after execution of the request.
    ///
    /// If the hook returns an error, the request will not be executed and the error will be
    /// returned instead.
    ///
    /// The hook can also modify the request context and the response. This could be used, for
    /// example, to enforce a maximum deadline on all requests.
    ///
    /// # Example
    ///
    /// ```rust
    /// use futures::{executor::block_on, future};
    /// use tarpc::{
    ///     context, ServerError,
    ///     server::{Serve, serve, request_hook::{BeforeRequest, AfterRequest, RequestHook}}
    /// };
    /// use std::{io, time::Instant};
    ///
    /// struct PrintLatency(Instant);
    ///
    /// impl<Req> BeforeRequest<Req> for PrintLatency {
    ///     async fn before(&mut self, _: &mut context::Context, _: &Req) -> Result<(), ServerError> {
    ///         self.0 = Instant::now();
    ///         Ok(())
    ///     }
    /// }
    ///
    /// impl<Resp> AfterRequest<Resp> for PrintLatency {
    ///     async fn after(
    ///         &mut self,
    ///         _: &mut context::Context,
    ///         _: &mut Result<Resp, ServerError>,
    ///     ) {
    ///         tracing::info!("Elapsed: {:?}", self.0.elapsed());
    ///     }
    /// }
    ///
    /// let serve = serve(|_ctx, i| async move {
    ///         Ok(i + 1)
    ///     }).before_and_after(PrintLatency(Instant::now()));
    /// let response = serve.serve(context::current(), 1);
    /// assert!(block_on(response).is_ok());
    /// ```
    fn before_and_after<Hook>(
        self,
        hook: Hook,
    ) -> HookThenServeThenHook<Self::Req, Self::Resp, Self, Hook>
    where
        Hook: BeforeRequest<Self::Req> + AfterRequest<Self::Resp>,
        Self: Sized,
    {
        HookThenServeThenHook::new(self, hook)
    }
}
impl<S: Serve> RequestHook for S {}
