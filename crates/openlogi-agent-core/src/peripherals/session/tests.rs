use super::*;
use openlogi_core::{config::Config, peripheral::EndpointId};

fn broker() -> Broker {
    let handle = Handle::new(&Config::ephemeral());
    handle.discover(Ok(Vec::new()));
    let sources = super::super::sources::Sources::new().unwrap();
    let record = sources
        .catalog
        .entries()
        .next()
        .unwrap()
        .record(
            SessionId {
                endpoint: EndpointId("synthetic".into()),
                generation: 1,
            },
            Vec::new(),
        )
        .unwrap();
    let (_, gate) = openlogi_hid::device_io_channel();
    let guard = Guard::new(handle, &record, None, gate);
    Broker::new(BTreeMap::new(), Vec::new(), guard)
}

#[tokio::test]
async fn canceled_broker_retains_ownership_until_blocking_native_work_finishes() {
    let mut broker = broker();
    let (started, starting) = tokio::sync::oneshot::channel();
    let (release, waiting) = std::sync::mpsc::channel();
    let (finished, finishing) = tokio::sync::oneshot::channel();
    let native = broker.guard.clone();
    broker.io.spawn(async move {
        tokio::task::spawn_blocking(move || {
            native.check().unwrap();
            started.send(()).unwrap();
            waiting.recv().unwrap();
            assert_eq!(native.check(), Err(PeripheralError::StaleSession));
            drop(native);
            finished.send(()).unwrap();
        })
        .await
        .unwrap();
    });
    starting.await.unwrap();
    let outstanding = broker.stop().await;
    drop(broker);
    assert!(
        outstanding.strong_count() > 0,
        "canceling an async task cannot release native I/O ownership"
    );
    release.send(()).unwrap();
    finishing.await.unwrap();
    assert_eq!(outstanding.strong_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn hid_deadline_reports_fault_before_native_work_releases_its_claim() {
    let mut broker = broker();
    let queue = broker.queue.clone();
    let (started, starting) = tokio::sync::oneshot::channel();
    let (release, waiting) = tokio::sync::oneshot::channel();
    let (finished, finishing) = tokio::sync::oneshot::channel();
    broker.io.spawn(async move {
        queue
            .complete_hid(1, async move {
                started.send(()).unwrap();
                waiting.await.unwrap();
                Ok(None)
            })
            .await;
        drop(queue);
        finished.send(()).unwrap();
    });
    let reporting = tokio::spawn(async move {
        let error = std::future::poll_fn(|cx| broker.poll(cx))
            .await
            .err()
            .expect("an uncertain request must fault the session");
        let outstanding = broker.stop().await;
        drop(broker);
        (error, outstanding)
    });
    starting.await.unwrap();
    tokio::time::advance(Duration::from_secs(3)).await;
    let (error, outstanding) = tokio::time::timeout(Duration::from_millis(1), reporting)
        .await
        .expect("the fault must wake the broker before native I/O completes")
        .unwrap();
    assert!(matches!(error, PeripheralError::WriteFailed(_)));
    assert!(
        outstanding.strong_count() > 0,
        "the deadline does not cancel submitted native work"
    );
    release.send(()).unwrap();
    finishing.await.unwrap();
    assert_eq!(outstanding.strong_count(), 0);
}
