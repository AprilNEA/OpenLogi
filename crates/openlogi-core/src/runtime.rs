//! The dedicated tokio thread OpenLogi's host crates run async work on.
//!
//! Neither GPUI process owns an async runtime, and the agent keeps its device
//! watchers and hardware writers off its main runtime, so each of them puts a
//! current-thread runtime on an OS thread of its own. That choice — which
//! runtime flavour, with which drivers, failing how — was once written out at
//! every such site; this module makes it once. Behind the `runtime` feature so
//! the portable core stays free of tokio.

use std::io;

use tokio::runtime::Runtime;

/// Build the current-thread runtime a dedicated worker runs on, with every
/// driver tokio was compiled with.
///
/// # Errors
///
/// Only if the OS refuses the runtime its resources — rare, and the caller's
/// to report in its own terms.
pub fn current_thread() -> io::Result<Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

/// Spawn a named OS thread that owns a current-thread runtime, and hand the
/// runtime to `run`.
///
/// The runtime is built before the thread starts, so a failure reaches the
/// caller instead of a thread that silently never ran. `run` typically blocks
/// on one future; when the order of teardown matters — a completion that must
/// not be acknowledged before the runtime has cancelled its tasks — `run` drops
/// the runtime explicitly before it reports.
///
/// # Errors
///
/// If the runtime or the thread cannot be created.
pub fn spawn_thread(name: &str, run: impl FnOnce(Runtime) + Send + 'static) -> io::Result<()> {
    let runtime = current_thread()?;
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || run(runtime))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worker_runs_its_future_on_the_named_thread() {
        let (report, reported) = std::sync::mpsc::channel();

        spawn_thread("openlogi-test-worker", move |runtime| {
            runtime.block_on(async {
                let _ = report.send(std::thread::current().name().map(str::to_owned));
            });
        })
        .expect("a worker thread starts");

        assert_eq!(
            reported.recv().expect("the worker reports").as_deref(),
            Some("openlogi-test-worker")
        );
    }
}
