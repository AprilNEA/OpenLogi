//! In-memory agents for client tests.
//!
//! A scripted agent over a tarpc channel instead of the socket: the client end
//! plugs straight into an [`AgentClient`], so the connect policy, the
//! generation ledger, and a client's own loop can be exercised with no
//! process, no socket file, and no hardware.

use std::future::Future;
use std::pin::{Pin, pin};

use futures_lite::StreamExt as _;
use tarpc::ServerError;
use tarpc::context::Context;
use tarpc::server::{BaseChannel, Channel as _};

use crate::{AgentClient, AgentRequest, AgentResponse};

/// One request's answer, boxed so a scripted handler can branch freely — and
/// hold a request open forever, as a quiet agent holds `observe`.
pub type Answer = Pin<Box<dyn Future<Output = Result<AgentResponse, ServerError>> + Send>>;

/// Serve `handle` over an in-memory channel until the client hangs up or
/// `until` resolves, and return the client end.
///
/// Requests are served concurrently, so a handler that never answers does not
/// block the next request. The server task runs on the tokio runtime the
/// calling test already has.
pub fn in_memory_agent<H>(
    handle: H,
    until: impl Future<Output = ()> + Send + 'static,
) -> AgentClient
where
    H: Fn(AgentRequest) -> Answer + Clone + Send + Sync + 'static,
{
    let (client_transport, server_transport) = tarpc::transport::channel::unbounded();
    let serve = tarpc::server::serve(move |_: Context, request: AgentRequest| handle(request));
    tokio::spawn(async move {
        let mut responses = pin!(BaseChannel::with_defaults(server_transport).execute(serve));
        let mut until = pin!(until);
        loop {
            tokio::select! {
                () = &mut until => break,
                next = responses.next() => match next {
                    Some(response) => {
                        tokio::spawn(response);
                    }
                    None => break,
                },
            }
        }
    });
    AgentClient::new(tarpc::client::Config::default(), client_transport).spawn()
}
