//! A G600 that records when each feature-report request reaches it, so the
//! tests can prove nothing reaches it while it stores a profile.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hidpp::channel::HidppChannel;
use openlogi_core::device::RawDeviceAddress;
use openlogi_core::hid::g600::{
    DpiSettings, DpiStage, DpiValue, G600Failure, OnboardProfile, StageAfterStore,
};
use openlogi_device_registry::g600::PROFILE_COMMIT_TIME as COMMIT;
use openlogi_hid::backend::{BackendError, HidBackend, HotplugStream, NodeId, NodeInfo, RawWriter};
use openlogi_hid::{DeviceRoute, WriteError, device_io_channel};
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::hardware::HardwareContext;

/// A G600 on profile 1, stage 1, whose writes land like the mouse's do.
struct G600 {
    node: NodeInfo,
    /// Whether a profile write lands but the host reports it failed, as when
    /// it delivered the report and lost the answer.
    profile_writes_fail: bool,
    reports: Mutex<HashMap<u8, Vec<u8>>>,
    /// Each request's report ID, whether it was a write, and when it began.
    requests: Mutex<Vec<(u8, bool, Instant)>>,
    profile_written: Notify,
}

impl G600 {
    /// A G600 with a node of its own, so tests running together share no
    /// route lock.
    fn new(name: &str) -> Self {
        let profile = |id: u8| {
            let mut report = vec![0; 154];
            report[0] = id;
            report[13] = 1;
            report[14..18].copy_from_slice(&[0x10, 0x20, 0, 0]);
            report
        };
        Self {
            node: NodeInfo {
                id: NodeId::from(name.to_owned()),
                vendor_id: 0x046d,
                product_id: 0xc24a,
                usage_page: 0xff80,
                usage_id: 0x0080,
                name: "G600 Gaming Mouse".into(),
                manufacturer: Some("Logitech".into()),
                serial_number: None,
            },
            profile_writes_fail: false,
            reports: Mutex::new(HashMap::from([
                (0xf0, vec![0xf0, 0x00, 0, 0]),
                (0xf3, profile(0xf3)),
                (0xf4, profile(0xf4)),
                (0xf5, profile(0xf5)),
            ])),
            requests: Mutex::new(Vec::new()),
            profile_written: Notify::new(),
        }
    }

    /// The route inventory gives this node, through its raw address.
    fn route(&self) -> DeviceRoute {
        DeviceRoute::from(&RawDeviceAddress {
            vendor_id: self.node.vendor_id,
            product_id: self.node.product_id,
            usage_page: self.node.usage_page,
            usage_id: self.node.usage_id,
            identity: self.node.identity(),
        })
    }

    fn record(&self, id: u8, write: bool) {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((id, write, Instant::now()));
    }

    fn requests(&self) -> Vec<(u8, bool, Instant)> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// When the profile report was written, and every request after it.
    fn after_profile_write(&self) -> (Instant, Vec<(u8, bool, Instant)>) {
        let requests = self.requests();
        let position = requests
            .iter()
            .position(|&(id, write, _)| write && id != 0xf0)
            .expect("a profile report was written");
        (requests[position].2, requests[position + 1..].to_vec())
    }
}

#[hidpp::async_trait]
impl HidBackend for G600 {
    async fn enumerate(&self) -> Result<Vec<NodeInfo>, BackendError> {
        Ok(vec![self.node.clone()])
    }

    async fn enumerate_hidpp(&self) -> Result<Vec<NodeInfo>, BackendError> {
        Ok(Vec::new())
    }

    async fn open_hidpp(
        &self,
        _node: &NodeInfo,
    ) -> Result<Option<Arc<HidppChannel>>, BackendError> {
        Ok(None)
    }

    async fn open_raw_writer(&self, _node: &NodeInfo) -> Result<Box<dyn RawWriter>, BackendError> {
        Err(BackendError::Backend("a G600 takes no raw writes".into()))
    }

    async fn read_feature_report(
        &self,
        _node: &NodeInfo,
        report: &mut [u8],
    ) -> Result<usize, BackendError> {
        self.record(report[0], false);
        let reports = self.reports.lock().unwrap_or_else(PoisonError::into_inner);
        let stored = reports
            .get(&report[0])
            .ok_or_else(|| BackendError::Backend("no such report".into()))?;
        report[..stored.len()].copy_from_slice(stored);
        Ok(stored.len())
    }

    async fn write_feature_report(
        &self,
        _node: &NodeInfo,
        report: &[u8],
    ) -> Result<(), BackendError> {
        self.record(report[0], true);
        let profile = report[0] != 0xf0;
        {
            let mut reports = self.reports.lock().unwrap_or_else(PoisonError::into_inner);
            if profile {
                reports.insert(report[0], report.to_vec());
            } else {
                let active = reports.get_mut(&0xf0).expect("an active report");
                active[1] = (active[1] & 0xf0) | (report[1] & 0b110);
            }
        }
        if profile {
            self.profile_written.notify_one();
            if self.profile_writes_fail {
                return Err(BackendError::Backend("pipe error".into()));
            }
        }
        Ok(())
    }

    fn watch(&self) -> Result<HotplugStream, BackendError> {
        Ok(Box::new(futures_lite::stream::empty()))
    }
}

fn stage(number: u8) -> DpiStage {
    DpiStage::from_number(number).expect("a stage")
}

fn new_settings() -> DpiSettings {
    let dpi = |value| Some(DpiValue::new(value).expect("a DPI"));
    DpiSettings::new([dpi(400), dpi(1600), None, None], stage(2)).expect("valid settings")
}

fn context(name: &str) -> (Arc<G600>, HardwareContext) {
    context_for(G600::new(name))
}

fn context_for(g600: G600) -> (Arc<G600>, HardwareContext) {
    let g600 = Arc::new(g600);
    let (_signal, device_io) = device_io_channel();
    let hardware = HardwareContext::injected(Arc::clone(&g600) as Arc<dyn HidBackend>, device_io);
    (g600, hardware)
}

#[tokio::test(start_paused = true)]
async fn a_read_waits_until_the_mouse_has_stored_a_profile_write() {
    let (g600, hardware) = context("read-behind-write");
    let route = g600.route();
    let profile = OnboardProfile::from_number(2).expect("a profile");

    let writing = tokio::spawn({
        let hardware = hardware.clone();
        let route = route.clone();
        async move {
            hardware
                .set_g600_profile_dpi(&route, profile, new_settings(), StageAfterStore::Keep)
                .await
        }
    });
    g600.profile_written.notified().await;
    let state = hardware
        .read_g600_dpi(&route)
        .await
        .expect("the read runs once the write is done");

    writing
        .await
        .expect("the write task finishes")
        .expect("the write is stored");
    let (written_at, after) = g600.after_profile_write();
    assert!(
        after.iter().all(|&(_, _, at)| at >= written_at + COMMIT),
        "a request reached the mouse while it stored the profile: {after:?}"
    );
    // Only the write's own read-back precedes the read's four reports.
    assert_eq!(
        after.iter().map(|&(id, _, _)| id).collect::<Vec<_>>(),
        [0xf4, 0xf0, 0xf3, 0xf4, 0xf5]
    );
    assert_eq!(state.profile(profile).dpi(stage(2)), Some(1600));
}

/// A profile write the host reports failed may still have reached the
/// mouse, so the next request waits out the commit as after a stored one.
#[tokio::test(start_paused = true)]
async fn a_read_waits_out_the_commit_of_a_profile_write_that_failed() {
    let (g600, hardware) = context_for(G600 {
        profile_writes_fail: true,
        ..G600::new("failed-write")
    });
    let route = g600.route();
    let profile = OnboardProfile::from_number(2).expect("a profile");

    let writing = tokio::spawn({
        let hardware = hardware.clone();
        let route = route.clone();
        async move {
            hardware
                .set_g600_profile_dpi(&route, profile, new_settings(), StageAfterStore::Keep)
                .await
        }
    });
    g600.profile_written.notified().await;
    hardware
        .read_g600_dpi(&route)
        .await
        .expect("the read runs once the write is done");

    let error = writing
        .await
        .expect("the write task finishes")
        .expect_err("the host reports the profile write failed");
    assert!(
        matches!(&error, WriteError::Hid(text) if text.contains("may still have applied it")),
        "{error:?}"
    );
    let (written_at, after) = g600.after_profile_write();
    assert_eq!(
        after.iter().map(|&(id, _, _)| id).collect::<Vec<_>>(),
        [0xf0, 0xf3, 0xf4, 0xf5],
        "only the queued read follows the failed write"
    );
    assert!(
        after.iter().all(|&(_, _, at)| at >= written_at + COMMIT),
        "a request reached the mouse while it may have stored the profile: {after:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_caller_that_stops_waiting_does_not_cut_the_commit_short() {
    let (g600, hardware) = context("abandoned-write");
    let route = g600.route();
    let profile = OnboardProfile::from_number(3).expect("a profile");

    let abandoned = tokio::time::timeout(Duration::from_millis(10), async {
        hardware
            .set_g600_profile_dpi(&route, profile, new_settings(), StageAfterStore::Keep)
            .await
    });
    // The caller gives up right after the profile report went out.
    let (abandoned, ()) = tokio::join!(abandoned, g600.profile_written.notified());
    assert!(
        abandoned.is_err(),
        "the caller stopped waiting before the commit"
    );
    hardware
        .set_g600_active_stage(&route, stage(2))
        .await
        .expect("the stage is enabled");

    let (written_at, after) = g600.after_profile_write();
    assert!(
        after.iter().all(|&(_, _, at)| at >= written_at + COMMIT),
        "a request reached the mouse while it stored the profile: {after:?}"
    );
    assert_eq!(
        after.first().map(|&(id, write, _)| (id, write)),
        Some((0xf5, false)),
        "the abandoned write still read its profile back first"
    );
}

/// The stage a write of the profile in use names is selected inside the
/// same locked operation: a request queued behind the write sees the mouse
/// on it, with no stage selection of its own in between.
#[tokio::test(start_paused = true)]
async fn a_named_stage_is_selected_before_a_queued_request_reaches_the_mouse() {
    let (g600, hardware) = context("named-stage");
    let route = g600.route();
    let in_use = OnboardProfile::from_number(1).expect("a profile");

    let writing = tokio::spawn({
        let hardware = hardware.clone();
        let route = route.clone();
        async move {
            hardware
                .set_g600_profile_dpi(
                    &route,
                    in_use,
                    new_settings(),
                    StageAfterStore::Use(stage(2)),
                )
                .await
        }
    });
    g600.profile_written.notified().await;
    let state = hardware
        .read_g600_dpi(&route)
        .await
        .expect("the read runs once the write is done");

    writing
        .await
        .expect("the write task finishes")
        .expect("the write is stored and stage 2 selected");
    let (written_at, after) = g600.after_profile_write();
    assert!(
        after.iter().all(|&(_, _, at)| at >= written_at + COMMIT),
        "a request reached the mouse while it stored the profile: {after:?}"
    );
    assert_eq!(
        after
            .iter()
            .map(|&(id, write, _)| (id, write))
            .collect::<Vec<_>>(),
        [
            (0xf3, false),
            (0xf0, true),
            (0xf0, false),
            (0xf0, false),
            (0xf3, false),
            (0xf4, false),
            (0xf5, false),
        ],
        "read-back, one stage selection and its check, then the queued read"
    );
    assert_eq!(state.active.dpi_stage, stage(2));
}

#[tokio::test]
async fn a_disabled_stage_is_refused_as_a_g600_failure() {
    let (g600, hardware) = context("disabled-stage");

    let error = hardware
        .set_g600_active_stage(&g600.route(), stage(4))
        .await
        .expect_err("stage 4 of profile 1 is off");

    assert_eq!(
        error,
        WriteError::G600(G600Failure::StageDisabled {
            profile: OnboardProfile::from_number(1).expect("a profile"),
            stage: stage(4),
        })
    );
    assert!(g600.requests().iter().all(|&(_, write, _)| !write));
}

#[tokio::test]
async fn nothing_reaches_the_mouse_while_the_host_suspends_device_io() {
    let g600 = Arc::new(G600::new("suspended"));
    let (signal, device_io) = device_io_channel();
    let hardware = HardwareContext::injected(Arc::clone(&g600) as Arc<dyn HidBackend>, device_io);
    assert!(signal.suspend());

    let error = hardware
        .read_g600_dpi(&g600.route())
        .await
        .expect_err("device I/O is suspended");

    assert_eq!(error, WriteError::DeviceNotFound);
    assert!(g600.requests().is_empty());
}
