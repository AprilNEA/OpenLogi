use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use tokio::sync::oneshot;

use super::{DeviceRoute, LightingJob, timed_out};

fn route(slot: u8) -> DeviceRoute {
    DeviceRoute::Unifying {
        receiver_uid: "lighting-worker-test".into(),
        slot,
    }
}

fn runtime() -> tokio::runtime::Runtime {
    openlogi_core::worker::runtime().unwrap()
}

#[test]
fn caller_runtime_destruction_does_not_drop_native_write_or_allow_successor_before_cleanup() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let first_events = Arc::clone(&events);
    let (started, started_rx) = mpsc::channel();
    let (finish_native, native_done) = oneshot::channel();
    let (restore_started, restoring) = mpsc::channel();
    let (finish_restore, restore_done) = oneshot::channel();
    let caller = runtime();
    caller.block_on(async {
        let job = LightingJob::spawn(&route(1), move |cancel| async move {
            first_events.lock().unwrap().push("claim submitted");
            started.send(()).unwrap();
            native_done.await.unwrap();
            first_events.lock().unwrap().push("claim completed");
            assert!(cancel.is_cancelled());
            restore_started.send(()).unwrap();
            restore_done.await.unwrap();
            first_events.lock().unwrap().push("restored");
            Err(timed_out())
        })
        .unwrap();
        // This does not depend on scheduling a Tokio task on `caller`.
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let error = tokio::time::timeout(Duration::from_millis(10), job.wait()).await;
        assert!(
            error.is_err(),
            "caller deadline should expire while native write is held"
        );
    });
    drop(caller);

    let successor_events = Arc::clone(&events);
    let (next_started, next_rx) = mpsc::channel();
    let successor = LightingJob::spawn(&route(1), move |_| async move {
        successor_events.lock().unwrap().push("successor");
        next_started.send(()).unwrap();
        Ok(())
    })
    .unwrap();
    assert!(next_rx.recv_timeout(Duration::from_millis(30)).is_err());
    assert_eq!(*events.lock().unwrap(), ["claim submitted"]);
    finish_native.send(()).unwrap();
    restoring.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(next_rx.recv_timeout(Duration::from_millis(30)).is_err());
    finish_restore.send(()).unwrap();
    runtime().block_on(successor.wait()).unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        [
            "claim submitted",
            "claim completed",
            "restored",
            "successor"
        ]
    );
}

#[test]
fn detached_reapply_survives_requester_and_does_not_cancel() {
    let (finish, proceed) = oneshot::channel();
    let (done, done_rx) = mpsc::channel();
    let job = LightingJob::spawn(&route(2), move |cancel| async move {
        proceed.await.unwrap();
        done.send(cancel.is_cancelled()).unwrap();
        Ok(())
    })
    .unwrap();
    job.detach();
    finish.send(()).unwrap();
    assert!(!done_rx.recv_timeout(Duration::from_secs(2)).unwrap());
}

#[test]
fn five_second_request_timeout_signals_but_retains_the_worker() {
    let (started, started_rx) = mpsc::channel();
    let (finish, proceed) = oneshot::channel();
    let (done, done_rx) = mpsc::channel();
    let job = LightingJob::spawn(&route(3), move |cancel| async move {
        started.send(()).unwrap();
        proceed.await.unwrap();
        done.send(cancel.is_cancelled()).unwrap();
        Ok(())
    })
    .unwrap();
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(runtime().block_on(job.wait()).unwrap_err(), timed_out());
    finish.send(()).unwrap();
    assert!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap());
}

#[test]
fn standalone_finish_does_not_return_at_the_request_deadline() {
    let (finish, proceed) = oneshot::channel();
    let (done, done_rx) = mpsc::channel();
    let job = LightingJob::spawn(&route(5), move |_| async move {
        proceed.await.unwrap();
        Err(timed_out())
    })
    .unwrap();
    let caller = std::thread::spawn(move || {
        done.send(runtime().block_on(job.finish())).unwrap();
    });
    assert!(matches!(
        done_rx.recv_timeout(Duration::from_millis(5100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    finish.send(()).unwrap();
    assert_eq!(
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        Err(timed_out())
    );
    caller.join().unwrap();
}

#[test]
fn cancelling_a_queued_job_does_not_run_its_operation() {
    let (started, started_rx) = mpsc::channel();
    let (finish, proceed) = oneshot::channel();
    let first = LightingJob::spawn(&route(4), move |_| async move {
        started.send(()).unwrap();
        proceed.await.unwrap();
        Ok(())
    })
    .unwrap();
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (ran, ran_rx) = mpsc::channel();
    let cancelled = LightingJob::spawn(&route(4), move |_| async move {
        ran.send(()).unwrap();
        Ok(())
    })
    .unwrap();
    drop(cancelled);
    finish.send(()).unwrap();
    runtime().block_on(first.wait()).unwrap();
    assert!(
        matches!(
            ran_rx.recv_timeout(Duration::from_secs(2)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ),
        "cancelled queued closure must be dropped without running"
    );
}
