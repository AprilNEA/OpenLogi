//! The G600 driver against a scripted mouse holding the reports captured from
//! a real G600 on Windows 11 (serial `783891C3F32F0017`, 2026-11-07).

use std::sync::{Arc, Mutex, PoisonError};

use hidpp::async_trait;
use hidpp::channel::HidppChannel;
use openlogi_core::device::RawDeviceAddress;
use openlogi_core::hid::Dpi;

use openlogi_device_registry::g600::G600_PRODUCT_ID;

use super::report::{ACTIVE_REPORT_LEN, FEATURE_REPORT_LEN};
use super::{dump_reports, get_dpi_info, is_g600_route, read_status, set_dpi};
use crate::backend::{
    BackendError, HidBackend, HotplugStream, NodeId, NodeInfo, RawFeatureHandle, RawWriter,
};
use crate::{DeviceRoute, WriteError};

/// Report `0xf0` as first read: profile 1, DPI slot 0, the power-on bit set.
const ACTIVE_CAPTURED: [u8; ACTIVE_REPORT_LEN] = [0xf0, 0x19, 0, 0];

/// Profile 0 as captured: LED off with the cycle effect, one 1200 DPI slot,
/// the thumb grid on the number row.
const PROFILE_0_CAPTURED: &str = "f300000002040000000000000001180000000000000000000200000000000001000002000003000004000005000000012800020514000000001e00001f00002000002100002200002300002400002500002600002700002d00002e00000001000002000003000004000005000017000000020514000000011e00011f00012000012100012200012300012400012500012600012700012d00012e";

/// Profile 1 as captured: white LED, solid, one 1200 DPI slot, the thumb
/// grid on F1–F12.
const PROFILE_1_CAPTURED: &str = "f4ffffff00000000000000000001180000000000000000000200000000000001000002000003000004000005000017000000020514000000005900005a00005b00005c00005d00005e00005f000060000061000062000056000057ffffff01000002000003000004000005000017000000020514000000015900015a00015b00015c00015d00015e00015f000160000161000162000156000157";

/// Profile 2 as captured, Logitech's stock "Generic Gaming" profile: green
/// LED breathing, DPI 400 / 1200 / 2000 / 3200 starting in the second slot,
/// DPI shift to 400.
const PROFILE_2_STOCK: &str = "f500ff0001040000000000000802081828400000000000000200000000000001000002000003000004000005000015000013000014000000001e00001f000020000021000022000023110000160000050000120000000008040000ffffff000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

fn report(hex: &str) -> [u8; FEATURE_REPORT_LEN] {
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("fixture hex"))
        .collect();
    bytes.try_into().expect("fixture length")
}

pub(super) fn profile_0_captured() -> [u8; FEATURE_REPORT_LEN] {
    report(PROFILE_0_CAPTURED)
}

pub(super) fn profile_1_captured() -> [u8; FEATURE_REPORT_LEN] {
    report(PROFILE_1_CAPTURED)
}

pub(super) fn profile_2_stock() -> [u8; FEATURE_REPORT_LEN] {
    report(PROFILE_2_STOCK)
}

#[derive(Clone, Copy)]
enum Mode {
    /// Answers reads from its reports and applies selects.
    Honest,
    /// Acknowledges every write and applies none.
    IgnoreWrites,
    /// Never answers.
    Hang,
}

/// A G600's reports, behind a feature handle.
struct Mouse {
    active: [u8; ACTIVE_REPORT_LEN],
    profiles: [[u8; FEATURE_REPORT_LEN]; 3],
    writes: Vec<Vec<u8>>,
    mode: Mode,
    /// Answer reads with the collection's full feature length, zero-padded,
    /// as Windows does, instead of the report's own length as hidraw does.
    pads_reads: bool,
}

impl Mouse {
    fn captured() -> Self {
        Self {
            active: ACTIVE_CAPTURED,
            profiles: [
                profile_0_captured(),
                profile_1_captured(),
                profile_2_stock(),
            ],
            writes: Vec::new(),
            mode: Mode::Honest,
            pads_reads: false,
        }
    }

    /// As captured, then with profile `profile` selected.
    fn on_profile(profile: u8) -> Self {
        let mut mouse = Self::captured();
        mouse.active[1] = 0x08 | (profile << 4);
        mouse
    }

    fn with_mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }

    fn padded(mut self) -> Self {
        self.pads_reads = true;
        self
    }

    fn select(&mut self, selection: u8) {
        if selection & 0x40 != 0 {
            self.active[1] = (self.active[1] & !0x06) | (selection & 0x06);
        }
        if selection & 0x80 != 0 {
            self.active[1] = (self.active[1] & !0xf0) | (selection & 0xf0);
        }
        // A select clears the power-on bit, as the real mouse does.
        self.active[1] &= !0x01;
    }

    fn slot(&self) -> u8 {
        (self.active[1] >> 1) & 0x03
    }
}

struct FeatureHandle(Arc<Mutex<Mouse>>);

impl FeatureHandle {
    fn hangs(&self) -> bool {
        matches!(
            self.0.lock().unwrap_or_else(PoisonError::into_inner).mode,
            Mode::Hang
        )
    }
}

#[async_trait]
impl RawFeatureHandle for FeatureHandle {
    async fn write_feature_report(&mut self, report: &[u8]) -> Result<(), BackendError> {
        if self.hangs() {
            return std::future::pending().await;
        }
        let mut mouse = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        mouse.writes.push(report.to_vec());
        if matches!(mouse.mode, Mode::IgnoreWrites) {
            return Ok(());
        }
        match *report {
            [0xf0, selection, ..] => mouse.select(selection),
            [id @ 0xf3..=0xf5, ..] => {
                let profile = &mut mouse.profiles[usize::from(id - 0xf3)];
                profile.copy_from_slice(&report[..FEATURE_REPORT_LEN]);
            }
            _ => return Err(BackendError::Backend("not a G600 report".into())),
        }
        Ok(())
    }

    async fn read_feature_report(&mut self, report: &mut [u8]) -> Result<usize, BackendError> {
        if self.hangs() {
            return std::future::pending().await;
        }
        let mouse = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let data: &[u8] = match report[0] {
            0xf0 => &mouse.active,
            id @ 0xf3..=0xf5 => &mouse.profiles[usize::from(id - 0xf3)],
            _ => return Err(BackendError::Backend("not a G600 report".into())),
        };
        let len = if mouse.pads_reads {
            report.len().min(FEATURE_REPORT_LEN)
        } else {
            data.len()
        };
        report[..data.len()].copy_from_slice(data);
        report[data.len()..len].fill(0);
        Ok(len)
    }
}

#[derive(Clone, Copy)]
enum Opens {
    Handle,
    Disconnected,
}

struct G600Backend {
    nodes: Vec<NodeInfo>,
    mouse: Arc<Mutex<Mouse>>,
    opens: Opens,
}

impl G600Backend {
    fn new(mouse: Mouse) -> Self {
        Self {
            nodes: vec![g600_node()],
            mouse: Arc::new(Mutex::new(mouse)),
            opens: Opens::Handle,
        }
    }

    fn disconnected(mut self) -> Self {
        self.opens = Opens::Disconnected;
        self
    }

    fn writes(&self) -> Vec<Vec<u8>> {
        self.mouse
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .writes
            .clone()
    }

    fn slot(&self) -> u8 {
        self.mouse
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slot()
    }
}

#[async_trait]
impl HidBackend for G600Backend {
    async fn enumerate(&self) -> Result<Vec<NodeInfo>, BackendError> {
        Ok(self.nodes.clone())
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
        Err(BackendError::Backend(
            "the G600 takes no output reports".into(),
        ))
    }

    async fn open_raw_feature(
        &self,
        _node: &NodeInfo,
    ) -> Result<Box<dyn RawFeatureHandle>, BackendError> {
        match self.opens {
            Opens::Handle => Ok(Box::new(FeatureHandle(Arc::clone(&self.mouse)))),
            Opens::Disconnected => Err(BackendError::Disconnected),
        }
    }

    fn watch(&self) -> Result<HotplugStream, BackendError> {
        Err(BackendError::Backend("no hotplug".into()))
    }
}

/// A backend that enumerates the G600 but leaves [`HidBackend::open_raw_feature`]
/// to its default.
struct NoFeatureBackend;

#[async_trait]
impl HidBackend for NoFeatureBackend {
    async fn enumerate(&self) -> Result<Vec<NodeInfo>, BackendError> {
        Ok(vec![g600_node()])
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
        Err(BackendError::Backend("no output reports".into()))
    }

    fn watch(&self) -> Result<HotplugStream, BackendError> {
        Err(BackendError::Backend("no hotplug".into()))
    }
}

fn g600_node() -> NodeInfo {
    NodeInfo {
        id: NodeId::from("g600-driver-test".to_owned()),
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: G600_PRODUCT_ID,
        usage_page: 0xff80,
        usage_id: 0x0080,
        name: "Gaming Mouse G600".into(),
        manufacturer: Some("Logitech".into()),
        serial_number: Some("783891C3F32F0017".into()),
    }
}

fn g600_route() -> DeviceRoute {
    let node = g600_node();
    DeviceRoute::from(&RawDeviceAddress {
        vendor_id: node.vendor_id,
        product_id: node.product_id,
        usage_page: node.usage_page,
        usage_id: node.usage_id,
        identity: node.identity(),
    })
}

fn dpi(value: u16) -> Dpi {
    Dpi::from(value)
}

#[test]
fn only_the_vendor_collection_of_a_g600_is_its_route() {
    assert!(is_g600_route(&g600_route()));
    let mouse_collection = DeviceRoute::RawHid {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: G600_PRODUCT_ID,
        usage_page: 0x0001,
        usage_id: 0x0002,
        identity: "serial:783891c3f32f0017".into(),
    };
    assert!(!is_g600_route(&mouse_collection));
    let litra = DeviceRoute::RawHid {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: 0xc900,
        usage_page: 0xff43,
        usage_id: 0x0202,
        identity: "serial:litra".into(),
    };
    assert!(!is_g600_route(&litra));
    assert!(!is_g600_route(&DeviceRoute::Direct {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: G600_PRODUCT_ID,
    }));
}

#[tokio::test]
async fn the_active_profiles_slots_are_the_dpi_table() {
    let backend = G600Backend::new(Mouse::on_profile(2));
    let info = get_dpi_info(&backend, &g600_route()).await.unwrap();
    assert_eq!(info.current, dpi(400));
    assert_eq!(info.capabilities.values(), [400, 1200, 2000, 3200].map(dpi));
    assert!(backend.writes().is_empty(), "a read writes nothing");
}

#[tokio::test]
async fn a_padded_mouse_reads_like_a_short_one() {
    let backend = G600Backend::new(Mouse::on_profile(2).padded());
    let info = get_dpi_info(&backend, &g600_route()).await.unwrap();
    assert_eq!(info.current, dpi(400));
    assert_eq!(info.capabilities.values().len(), 4);
}

#[tokio::test]
async fn a_profile_with_one_slot_lists_only_it() {
    // As captured: profile 1 holds 1200 and nothing else.
    let backend = G600Backend::new(Mouse::captured());
    let info = get_dpi_info(&backend, &g600_route()).await.unwrap();
    assert_eq!(info.current, dpi(1200));
    assert_eq!(info.capabilities.values(), [dpi(1200)]);
}

#[tokio::test]
async fn a_disabled_active_slot_falls_back_to_the_default_slot() {
    let mut mouse = Mouse::captured();
    // Slot 3 of profile 1, which the profile disables.
    mouse.select(0x46);
    let backend = G600Backend::new(mouse);
    let info = get_dpi_info(&backend, &g600_route()).await.unwrap();
    assert_eq!(info.current, dpi(1200));
}

#[tokio::test]
async fn a_profile_with_every_slot_disabled_has_no_dpi_list() {
    let mut mouse = Mouse::on_profile(2);
    mouse.profiles[2][14..18].fill(0);
    let backend = G600Backend::new(mouse);
    let error = get_dpi_info(&backend, &g600_route()).await.unwrap_err();
    assert!(matches!(error, WriteError::EmptyDpiList), "{error:?}");
}

#[tokio::test]
async fn setting_a_dpi_selects_the_slot_that_holds_it() {
    let backend = G600Backend::new(Mouse::on_profile(2));
    set_dpi(&backend, &g600_route(), dpi(2000)).await.unwrap();
    assert_eq!(backend.writes(), [vec![0xf0, 0x44, 0, 0]]);
    assert_eq!(backend.slot(), 2);
    let info = get_dpi_info(&backend, &g600_route()).await.unwrap();
    assert_eq!(info.current, dpi(2000));
}

#[tokio::test]
async fn a_dpi_no_slot_holds_is_refused_without_a_write() {
    let backend = G600Backend::new(Mouse::on_profile(2));
    let error = set_dpi(&backend, &g600_route(), dpi(1000))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        WriteError::DpiUnsupported {
            requested: dpi(1000)
        }
    );
    assert!(backend.writes().is_empty());
}

#[tokio::test]
async fn a_select_the_mouse_ignores_is_tried_once_more_then_reported() {
    let backend = G600Backend::new(Mouse::on_profile(2).with_mode(Mode::IgnoreWrites));
    let error = set_dpi(&backend, &g600_route(), dpi(2000))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        WriteError::DpiNotApplied {
            requested: dpi(2000),
            actual: dpi(400),
        }
    );
    assert_eq!(backend.writes().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_mouse_that_does_not_answer_times_out() {
    let backend = G600Backend::new(Mouse::on_profile(2).with_mode(Mode::Hang));
    let error = get_dpi_info(&backend, &g600_route()).await.unwrap_err();
    assert!(
        matches!(error, WriteError::RequestTimedOut { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_route_that_is_not_the_g600s_finds_no_device() {
    let backend = G600Backend::new(Mouse::captured());
    let litra = DeviceRoute::RawHid {
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: 0xc900,
        usage_page: 0xff43,
        usage_id: 0x0202,
        identity: "serial:litra".into(),
    };
    let error = get_dpi_info(&backend, &litra).await.unwrap_err();
    assert_eq!(error, WriteError::DeviceNotFound);
    assert!(backend.writes().is_empty());
}

#[tokio::test]
async fn a_g600_that_is_not_connected_is_not_found() {
    let mut backend = G600Backend::new(Mouse::captured());
    backend.nodes.clear();
    let error = get_dpi_info(&backend, &g600_route()).await.unwrap_err();
    assert_eq!(error, WriteError::DeviceNotFound);
}

#[tokio::test]
async fn a_handle_the_backend_cannot_open_is_a_transport_error() {
    let backend = G600Backend::new(Mouse::captured()).disconnected();
    let error = get_dpi_info(&backend, &g600_route()).await.unwrap_err();
    assert!(matches!(error, WriteError::Hid(_)), "{error:?}");
}

#[tokio::test]
async fn a_backend_without_feature_reports_says_so() {
    let error = get_dpi_info(&NoFeatureBackend, &g600_route())
        .await
        .unwrap_err();
    let WriteError::Hid(message) = error else {
        panic!("expected a transport error, got {error:?}");
    };
    assert!(message.contains("feature-report"), "{message}");
}

#[tokio::test]
async fn the_status_reads_every_profile() {
    let backend = G600Backend::new(Mouse::captured());
    let status = read_status(&backend, &g600_route()).await.unwrap();
    assert_eq!(status.active_profile, 1);
    assert_eq!(status.active_dpi_slot, 0);
    let stock = &status.profiles[2];
    assert_eq!(
        stock.dpi_slots,
        [400, 1200, 2000, 3200].map(|value| Some(dpi(value)))
    );
    assert_eq!(stock.default_dpi_slot, Some(1));
    assert_eq!(stock.dpi_shift, Some(dpi(400)));
    assert_eq!(stock.report_rate_hz, 1000);
    assert_eq!(stock.led, [0x00, 0xff, 0x00]);
    let first = &status.profiles[0];
    assert_eq!(first.dpi_slots, [Some(dpi(1200)), None, None, None]);
    assert_eq!(first.default_dpi_slot, Some(0));
    assert_eq!(first.dpi_shift, None);
    assert_eq!(first.led, [0, 0, 0]);
}

#[tokio::test]
async fn the_dump_is_the_reports_verbatim() {
    let backend = G600Backend::new(Mouse::captured().padded());
    let dump = dump_reports(&backend, &g600_route()).await.unwrap();
    assert_eq!(dump.active, ACTIVE_CAPTURED);
    assert_eq!(
        dump.profiles,
        [
            profile_0_captured(),
            profile_1_captured(),
            profile_2_stock()
        ]
    );
}
