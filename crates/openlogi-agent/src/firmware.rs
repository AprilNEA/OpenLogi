//! Agent-owned firmware jobs, independent of the requesting IPC connection.
//!
//! See `docs/FIRMWARE_UPDATES.md` at the repository root for the installation
//! lifecycle, exclusive I/O ownership and release-discovery limitations.

mod download;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use openlogi_agent_core::observable::ObservableState;
use openlogi_agent_core::orchestrator::SharedHandles;
use openlogi_core::hid::{
    DeviceRoute, FirmwareStatus, FirmwareUpdate, FirmwareUpdateFailure, FirmwareUpdatePhase,
    FirmwareUpdateRefusal, FirmwareVersion, HidppOperation, WriteError,
};
use openlogi_device::firmware;
use openlogi_device_registry::firmware::FirmwareRelease;
use tokio::sync::{Mutex, OwnedMutexGuard, watch};

const RESTART_TIMEOUT: Duration = Duration::from_secs(90);

/// Admits one firmware job at a time and publishes its complete lifecycle.
pub(crate) struct FirmwareManager {
    admission: Arc<Mutex<()>>,
    stopping: watch::Sender<bool>,
    next_id: AtomicU64,
    shared: SharedHandles,
    observable: Arc<ObservableState>,
}

impl FirmwareManager {
    pub(crate) fn new(shared: SharedHandles, observable: Arc<ObservableState>) -> Self {
        Self {
            admission: Arc::default(),
            stopping: watch::channel(false).0,
            next_id: AtomicU64::new(1),
            shared,
            observable,
        }
    }

    pub(crate) async fn status(&self, route: &DeviceRoute) -> Result<FirmwareStatus, WriteError> {
        self.shared
            .device(route)
            .run(HidppOperation::ReadFirmware, |channel| async move {
                firmware::get_firmware_status_on(&channel).await
            })
            .await
    }

    pub(crate) async fn start(
        self: &Arc<Self>,
        route: DeviceRoute,
    ) -> Result<(), FirmwareUpdateRefusal> {
        let admission = admit(&self.admission)?;
        if *self.stopping.borrow() {
            return Err(FirmwareUpdateRefusal::Busy);
        }
        let target = self
            .shared
            .device(&route)
            .run(HidppOperation::ReadFirmware, |channel| async move {
                firmware::update_target_on(&channel).await
            })
            .await
            .map_err(|_| FirmwareUpdateRefusal::DeviceNotFound)?
            .ok_or(FirmwareUpdateRefusal::NoUpdate)?;
        if *self.stopping.borrow() {
            return Err(FirmwareUpdateRefusal::Busy);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let release = target.release();
        self.publish(id, &route, release, FirmwareUpdatePhase::Downloading);
        // Ownership moves to the agent before acknowledging admission. Client
        // cancellation/disconnect cannot cancel a flash transaction.
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            manager.run(id, route, target, admission).await;
        });
        Ok(())
    }

    async fn run(
        &self,
        id: u64,
        route: DeviceRoute,
        target: firmware::FirmwareUpdateTarget,
        _admission: OwnedMutexGuard<()>,
    ) {
        // The supervisor publishes a terminal result even if the worker panics;
        // admission remains held until that publication and resource cleanup.
        let release = target.release();
        let shared = self.shared.clone();
        let observable = Arc::clone(&self.observable);
        let worker_route = route.clone();
        let stopping = self.stopping.subscribe();
        let result = tokio::spawn(async move {
            run_update(shared, observable, id, worker_route, target, stopping).await
        })
        .await
        .unwrap_or_else(|error| Err(FirmwareUpdateFailure::Protocol(error.to_string())));
        let phase = match result {
            Ok(()) => FirmwareUpdatePhase::Succeeded,
            Err(error) => {
                tracing::warn!(%route, %error, "firmware update stopped");
                FirmwareUpdatePhase::Failed(error)
            }
        };
        self.publish(id, &route, release, phase);
    }

    /// Refuse new jobs and wait for cooperative stop, native-write completion,
    /// terminal publication, and lease cleanup before the process can leave.
    pub(crate) async fn stop(&self) {
        self.stopping.send_replace(true);
        let _drained = self.admission.lock().await;
    }

    /// Resume admission after an attempted executable replacement failed.
    pub(crate) fn resume(&self) {
        self.stopping.send_replace(false);
    }

    fn publish(
        &self,
        id: u64,
        route: &DeviceRoute,
        release: &FirmwareRelease,
        phase: FirmwareUpdatePhase,
    ) {
        publish(&self.observable, id, route, release, phase);
    }
}

fn admit(admission: &Arc<Mutex<()>>) -> Result<OwnedMutexGuard<()>, FirmwareUpdateRefusal> {
    Arc::clone(admission)
        .try_lock_owned()
        .map_err(|_| FirmwareUpdateRefusal::Busy)
}

fn publish(
    observable: &ObservableState,
    id: u64,
    route: &DeviceRoute,
    release: &FirmwareRelease,
    phase: FirmwareUpdatePhase,
) {
    observable.set_firmware_update(FirmwareUpdate {
        source: Some(release.into()),
        events: Vec::new(),
        id,
        route: route.clone(),
        target: FirmwareVersion {
            number: release.number,
            revision: release.revision,
            build: release.build,
        },
        phase,
    });
}

async fn run_update(
    shared: SharedHandles,
    observable: Arc<ObservableState>,
    id: u64,
    route: DeviceRoute,
    target: firmware::FirmwareUpdateTarget,
    mut stopping: watch::Receiver<bool>,
) -> Result<(), FirmwareUpdateFailure> {
    let release = target.release();
    let download = tokio::task::spawn_blocking(move || download::download(release));
    // Dropping a download join handle is safe: the blocking worker only owns
    // HTTP bytes, never a device. A native flash write is always drained below.
    let package = tokio::select! {
        biased;
        _ = stopping.wait_for(|stopping| *stopping) => return Err(FirmwareUpdateFailure::Protocol("agent is stopping".into())),
        result = download => result.map_err(|error| FirmwareUpdateFailure::Download(error.to_string()))??,
    };
    publish(
        &observable,
        id,
        &route,
        release,
        FirmwareUpdatePhase::Preparing,
    );
    let gate = shared.device_io.clone();
    let transfer_stop = stopping.clone();
    let progress = Arc::clone(&observable);
    let progress_route = route.clone();
    let pending = shared
        .device(&route)
        .firmware(|channel| async move {
            let prepared = firmware::prepare_update_on(&channel, target, &package).await?;
            prepared
                .install(
                    |phase| publish(&progress, id, &progress_route, release, phase),
                    || !gate.allows_io() || *transfer_stop.borrow(),
                )
                .await
        })
        .await?;
    // The old feature/channel is gone before inventory is asked to reconnect.
    publish(
        &observable,
        id,
        &route,
        release,
        FirmwareUpdatePhase::Restarting,
    );
    shared.request_receiver_rescan();
    let deadline = tokio::time::Instant::now() + RESTART_TIMEOUT;
    loop {
        if !shared.device_io.allows_io() || *stopping.borrow() {
            return Err(FirmwareUpdateFailure::NotActivated);
        }
        let verification = tokio::time::timeout_at(
            deadline,
            shared
                .device(&route)
                .run(HidppOperation::ReadFirmware, |channel| {
                    let pending = &pending;
                    async move {
                        // Keep the device's typed verification result inside the I/O
                        // seam's bounded channel lookup/request budget.
                        Ok(pending.verify_on(&channel).await)
                    }
                }),
        )
        .await
        .unwrap_or(Err(WriteError::RequestTimedOut {
            operation: HidppOperation::ReadFirmware,
        }));
        match verification {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(FirmwareUpdateFailure::WrongDevice)) => {
                return Err(FirmwareUpdateFailure::WrongDevice);
            }
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(FirmwareUpdateFailure::NotActivated);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_stays_busy_until_the_job_releases_ownership() {
        let slot = Arc::new(Mutex::new(()));
        let admitted = admit(&slot).unwrap();
        assert!(matches!(admit(&slot), Err(FirmwareUpdateRefusal::Busy)));
        drop(admitted);
        let _retry = admit(&slot).expect("terminal cleanup releases admission");
    }
    #[tokio::test]
    async fn shutdown_closes_admission_and_drains_the_active_owner_before_returning() {
        let observable = Arc::new(ObservableState::new("test".into()));
        let shared = openlogi_agent_core::orchestrator::Orchestrator::new(
            openlogi_core::config::Config::default(),
            Arc::clone(&observable),
        )
        .shared();
        let manager = Arc::new(FirmwareManager::new(shared, observable));
        let job = admit(&manager.admission).unwrap();
        let mut stopping = manager.stopping.subscribe();
        let stop = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move {
                manager.stop().await;
            }
        });
        stopping.wait_for(|stopping| *stopping).await.unwrap();
        assert!(
            !stop.is_finished(),
            "shutdown waits for native-write ownership cleanup"
        );
        let route = DeviceRoute::Bolt {
            receiver_uid: "missing".into(),
            slot: 1,
        };
        assert_eq!(
            manager.start(route.clone()).await,
            Err(FirmwareUpdateRefusal::Busy)
        );
        drop(job);
        stop.await.unwrap();
        assert_eq!(
            manager.start(route.clone()).await,
            Err(FirmwareUpdateRefusal::Busy)
        );
        manager.resume();
        assert_eq!(
            manager.start(route).await,
            Err(FirmwareUpdateRefusal::DeviceNotFound)
        );
    }
}
