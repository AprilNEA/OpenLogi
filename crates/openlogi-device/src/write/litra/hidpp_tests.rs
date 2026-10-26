//! Tests for reaching a Litra over HID++ `0x1990` and falling back to its
//! fixed raw report.
//!
//! The scripted light answers `0x1990` at index 6 rather than the 4 the raw
//! report hard-codes, so a test that passes proves the index came from the
//! root feature.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hidpp::channel::HidppChannel;

use super::illumination::{RESOLVE_TIMEOUT, control_range};
use super::{
    LightChange, LightCommand, LightControls, LightWatch, LitraModel, apply, encode_command,
    read_state, watch,
};
use crate::backend::{BackendError, HidBackend, HotplugStream, NodeId, NodeInfo, RawWriter};
use crate::channel::route::DeviceRoute;
use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};
use crate::write::{HidppFeatureErrorKind, HidppOperation, WriteError};
use openlogi_core::device::{LightValueUnit, RawDeviceAddress};

/// Where the scripted light lists `0x1990`.
const INDEX: u8 = 0x06;

/// `HAS_EVENTS | HAS_LINEAR_LEVELS`.
const LINEAR: u8 = 0b0011;
/// [`LINEAR`] plus `HAS_DYNAMIC_MAXIMUM`.
const DYNAMIC: u8 = 0b1011;

/// How a scripted light departs from a well-behaved one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Quirk {
    None,
    /// The root lookup does not list `0x1990`.
    NoIllumination,
    /// `getBrightnessInfo` answers with a HID++ error.
    RejectsReads,
    /// Setters are acknowledged without changing anything.
    IgnoresWrites,
    /// Setters answer with a HID++ error.
    RejectsWrites,
    /// Nothing is answered any more, as after the light was unplugged.
    Gone,
    /// Setters change the light without announcing it.
    Unannounced,
}

/// A scripted Litra's feature table and state.
#[derive(Clone)]
struct Light {
    quirk: Quirk,
    version: u8,
    brightness_flags: u8,
    brightness: (u16, u16, u16),
    temperature: (u16, u16, u16),
    on: bool,
    lumens: u16,
    kelvin: u16,
    /// `getBrightnessEffectiveMax`; `0` is no limit. A brighter request is
    /// lowered to it, as the specification describes.
    effective_max: u16,
    /// Every `0x1990` request, as `(function, first parameter bytes)`.
    requests: Vec<(u8, [u8; 2])>,
    /// Where the light's notifications go once a HID++ channel is open.
    notify: Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>,
    /// Brightnesses the button sets while the light answers
    /// `getColorTemperature`, one per answer, each announced before the
    /// answer: a read that already passed the brightness misses it.
    presses_during_reads: Vec<u16>,
}

impl Light {
    /// A Litra Glow as litra-rs describes it, on, at 100 lm and 4000 K.
    fn glow() -> Self {
        Self {
            quirk: Quirk::None,
            version: 1,
            brightness_flags: LINEAR,
            brightness: (20, 250, 1),
            temperature: (2700, 6500, 100),
            on: true,
            lumens: 100,
            kelvin: 4000,
            effective_max: 0,
            requests: Vec::new(),
            notify: None,
            presses_during_reads: Vec::new(),
        }
    }

    /// Send the `0x1990` notification `sub_id` with `payload`, as the light
    /// does after any change, including one OpenLogi asked for.
    fn announce(&self, sub_id: u8, payload: &[u8]) {
        let Some(notify) = &self.notify else {
            return;
        };
        let mut report = vec![0u8; 20];
        report[0] = 0x11;
        report[1] = 0xff;
        report[2] = INDEX;
        report[3] = sub_id << 4;
        report[4..4 + payload.len()].copy_from_slice(payload);
        let _ = notify.send(report);
    }

    /// Announce a write OpenLogi asked for, unless this light does not.
    fn echo(&self, sub_id: u8, payload: &[u8]) {
        if self.quirk != Quirk::Unannounced {
            self.announce(sub_id, payload);
        }
    }

    fn functions(&self) -> Vec<u8> {
        self.requests
            .iter()
            .map(|(function, _)| *function)
            .collect()
    }

    fn respond(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        if self.quirk == Quirk::Gone {
            return None;
        }
        // A directly attached device answers at `0xff` only.
        if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) || request[1] != 0xff {
            return None;
        }
        let function = request[3] >> 4;
        let argument = u16::from_be_bytes([request[4], request[5]]);
        let mut payload = [0u8; 16];
        match (request[2], function) {
            // Root ping, then the feature lookup.
            (0x00, 0x01) => payload[0] = 4,
            (0x00, 0x00) => {
                if argument == 0x1990 && self.quirk != Quirk::NoIllumination {
                    payload[0] = INDEX;
                    payload[2] = self.version;
                }
            }
            (INDEX, function) => {
                self.requests.push((function, [request[4], request[5]]));
                if function == 2 && self.quirk == Quirk::RejectsReads {
                    // ERR_INVALID_FUNCTION_ID.
                    return Some(feature_error(request, 0x07));
                }
                let setter = matches!(function, 1 | 4 | 9);
                if setter && self.quirk == Quirk::RejectsWrites {
                    // ERR_INVALID_ARGUMENT.
                    return Some(feature_error(request, 0x02));
                }
                match function {
                    _ if setter && self.quirk == Quirk::IgnoresWrites => {}
                    0 => payload[0] = u8::from(self.on),
                    1 => {
                        self.on = request[4] & 1 != 0;
                        self.echo(0, &[u8::from(self.on)]);
                    }
                    2 => info(&mut payload, self.brightness_flags, self.brightness),
                    3 => payload[..2].copy_from_slice(&self.lumens.to_be_bytes()),
                    4 => {
                        self.lumens = match self.effective_max {
                            0 => argument,
                            limit => argument.min(limit),
                        };
                        self.echo(1, &self.lumens.to_be_bytes());
                    }
                    7 => info(&mut payload, LINEAR, self.temperature),
                    8 => {
                        if !self.presses_during_reads.is_empty() {
                            self.lumens = self.presses_during_reads.remove(0);
                            self.announce(1, &self.lumens.to_be_bytes());
                        }
                        payload[..2].copy_from_slice(&self.kelvin.to_be_bytes());
                    }
                    9 => {
                        self.kelvin = argument;
                        self.echo(2, &self.kelvin.to_be_bytes());
                    }
                    12 => payload[..2].copy_from_slice(&self.effective_max.to_be_bytes()),
                    _ => return None,
                }
            }
            _ => return None,
        }
        let mut response = vec![0u8; 20];
        response[0] = 0x11;
        response[1..4].copy_from_slice(&request[1..4]);
        response[4..].copy_from_slice(&payload);
        Some(response)
    }
}

/// A `get<Control>Info` reply: flags, then big-endian min, max and resolution.
fn info(payload: &mut [u8; 16], flags: u8, (min, max, resolution): (u16, u16, u16)) {
    payload[0] = flags;
    payload[1..3].copy_from_slice(&min.to_be_bytes());
    payload[3..5].copy_from_slice(&max.to_be_bytes());
    payload[5..7].copy_from_slice(&resolution.to_be_bytes());
}

/// How the backend answers a HID++ open of the light's node.
#[derive(Clone, Copy)]
enum Opens {
    /// A channel to the scripted [`Light`].
    Hidpp,
    /// A channel to a device that never answers.
    Silent,
    /// The node carries no HID++ collection.
    NotHidpp,
    /// The backend refuses the open this time.
    Fails,
}

/// A backend with Litra nodes, a capturing raw writer and a scripted light.
struct LitraBackend {
    nodes: Vec<NodeInfo>,
    opens: Opens,
    light: Arc<Mutex<Light>>,
    hidpp_opens: AtomicUsize,
    raw_reports: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl LitraBackend {
    fn new(tag: &str, opens: Opens, light: Light) -> Self {
        Self {
            nodes: vec![litra_node(tag)],
            opens,
            light: Arc::new(Mutex::new(light)),
            hidpp_opens: AtomicUsize::new(0),
            raw_reports: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The route the standalone inventory publishes for the first node.
    fn route(&self) -> DeviceRoute {
        let node = &self.nodes[0];
        DeviceRoute::from(&RawDeviceAddress {
            vendor_id: node.vendor_id,
            product_id: node.product_id,
            usage_page: node.usage_page,
            usage_id: node.usage_id,
            identity: node.identity(),
        })
    }

    fn light(&self) -> Light {
        self.light
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn raw_reports(&self) -> Vec<Vec<u8>> {
        self.raw_reports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn hidpp_opens(&self) -> usize {
        self.hidpp_opens.load(Ordering::SeqCst)
    }

    /// Change the light as its own buttons do, and announce notification
    /// `sub_id` with the payload `change` returns.
    fn press(&self, sub_id: u8, change: impl FnOnce(&mut Light) -> Vec<u8>) {
        let mut light = self.light.lock().unwrap_or_else(PoisonError::into_inner);
        let payload = change(&mut light);
        light.announce(sub_id, &payload);
    }

    /// Change the light the way `change` does without announcing it yet.
    fn change_silently(&self, change: impl FnOnce(&mut Light)) {
        change(&mut self.light.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

/// A Litra Glow node whose OS id and serial are unique to `tag`, so tests
/// never share a remembered raw-only answer.
fn litra_node(tag: &str) -> NodeInfo {
    NodeInfo {
        id: NodeId::from(format!("litra-hidpp-test-{tag}")),
        vendor_id: crate::LOGITECH_VENDOR_ID,
        product_id: super::LITRA_GLOW_PRODUCT_ID,
        usage_page: 0xff43,
        usage_id: 0x0202,
        name: "Litra Glow".into(),
        manufacturer: Some("Logitech".into()),
        serial_number: Some(format!("LITRA-{tag}")),
    }
}

struct CapturingWriter(Arc<Mutex<Vec<Vec<u8>>>>);

#[hidpp::async_trait]
impl RawWriter for CapturingWriter {
    async fn write_output_report(&mut self, report: &[u8]) -> Result<(), BackendError> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(report.to_vec());
        Ok(())
    }
}

#[hidpp::async_trait]
impl HidBackend for LitraBackend {
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
        self.hidpp_opens.fetch_add(1, Ordering::SeqCst);
        match self.opens {
            Opens::Hidpp => {
                let light = Arc::clone(&self.light);
                let (raw, _) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
                    light
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .respond(request)
                });
                self.light
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .notify = Some(raw.incoming_sender());
                Ok(Some(scripted_channel(raw).await))
            }
            Opens::Silent => {
                let (raw, _) = ScriptedRawHidChannel::with_responder(|_| None);
                Ok(Some(scripted_channel(raw).await))
            }
            Opens::NotHidpp => Ok(None),
            Opens::Fails => Err(BackendError::Backend("busy".into())),
        }
    }

    async fn open_raw_writer(&self, _node: &NodeInfo) -> Result<Box<dyn RawWriter>, BackendError> {
        Ok(Box::new(CapturingWriter(Arc::clone(&self.raw_reports))))
    }

    fn watch(&self) -> Result<HotplugStream, BackendError> {
        Ok(Box::new(futures_lite::stream::empty()))
    }
}

#[tokio::test]
async fn reads_the_light_through_the_index_its_root_feature_reports() {
    let backend = LitraBackend::new("read", Opens::Hidpp, Light::glow());

    let state = read_state(&backend, &backend.route(), LitraModel::Glow)
        .await
        .expect("the light answers")
        .expect("a HID++ light reads back");

    assert!(state.on);
    assert_eq!(state.brightness, Some(100));
    assert_eq!(state.temperature_kelvin, Some(4000));
    assert_eq!(state.brightness_limit, None);
    let brightness = state.capabilities.brightness.expect("a brightness range");
    assert_eq!((brightness.min(), brightness.max()), (20, 250));
    let temperature = state.capabilities.temperature.expect("a temperature range");
    assert_eq!(
        (temperature.min(), temperature.max(), temperature.step()),
        (2700, 6500, 100)
    );
    assert!(backend.raw_reports().is_empty(), "nothing goes out raw");
    // Brightness info first, as Logi Options+ reads it; no effective max
    // without the dynamic-maximum flag.
    assert_eq!(backend.light().functions(), [2, 7, 0, 3, 8]);
}

#[tokio::test]
async fn the_effective_maximum_needs_version_one_and_the_dynamic_maximum_flag() {
    let cases = [
        // (version, flags, effective max, expected limit, F12 asked)
        (0, DYNAMIC, 150, None, false),
        (1, LINEAR, 150, None, false),
        (1, DYNAMIC, 150, Some(150), true),
        (1, DYNAMIC, 0, None, true),
        (1, DYNAMIC, 250, None, true),
    ];
    for (case, (version, flags, effective_max, limit, asked)) in cases.into_iter().enumerate() {
        let light = Light {
            version,
            brightness_flags: flags,
            effective_max,
            ..Light::glow()
        };
        let backend = LitraBackend::new(&format!("limit-{case}"), Opens::Hidpp, light);
        let state = read_state(&backend, &backend.route(), LitraModel::Glow)
            .await
            .expect("the light answers")
            .expect("a HID++ light reads back");
        assert_eq!(state.brightness_limit, limit, "case {case}");
        assert_eq!(
            backend.light().functions().contains(&12),
            asked,
            "case {case}"
        );
    }
}

#[tokio::test]
async fn a_write_goes_to_the_resolved_feature_and_is_read_back() {
    let backend = LitraBackend::new("write", Opens::Hidpp, Light::glow());

    let state = apply(
        &backend,
        &backend.route(),
        LitraModel::Glow,
        LightCommand::BrightnessPercent(50),
    )
    .await
    .expect("the write succeeds")
    .expect("a HID++ light reads back");

    // 50 % of 20–250 lm is 135 lm, as the raw report encodes it.
    assert_eq!(state.brightness, Some(135));
    let light = backend.light();
    assert_eq!(light.lumens, 135);
    let writes: Vec<_> = light
        .requests
        .iter()
        .filter(|(function, _)| *function == 4)
        .collect();
    assert_eq!(writes, [&(4, 135u16.to_be_bytes())]);
    assert_eq!(light.functions().last(), Some(&3), "the write is read back");
    assert!(backend.raw_reports().is_empty());
}

#[tokio::test]
async fn a_brightness_above_the_limit_comes_back_as_the_value_the_light_kept() {
    let light = Light {
        brightness_flags: DYNAMIC,
        effective_max: 150,
        ..Light::glow()
    };
    let backend = LitraBackend::new("clamp", Opens::Hidpp, light);

    let state = apply(
        &backend,
        &backend.route(),
        LitraModel::Glow,
        LightCommand::BrightnessNative(200),
    )
    .await
    .expect("a clamped write is not an error")
    .expect("a HID++ light reads back");

    assert_eq!(state.brightness, Some(150));
    assert_eq!(state.brightness_limit, Some(150));
}

#[tokio::test]
async fn a_value_the_light_already_holds_is_not_written() {
    let backend = LitraBackend::new("same", Opens::Hidpp, Light::glow());
    let route = backend.route();

    for command in [
        LightCommand::Power(true),
        LightCommand::BrightnessNative(100),
        LightCommand::TemperatureKelvin(4000),
    ] {
        apply(&backend, &route, LitraModel::Glow, command)
            .await
            .expect("the light answers");
    }

    let functions = backend.light().functions();
    assert!(
        !functions
            .iter()
            .any(|function| matches!(function, 1 | 4 | 9)),
        "no setter ran: {functions:?}"
    );
}

#[tokio::test]
async fn power_and_temperature_writes_are_read_back() {
    let backend = LitraBackend::new("power", Opens::Hidpp, Light::glow());
    let route = backend.route();

    let off = apply(
        &backend,
        &route,
        LitraModel::Glow,
        LightCommand::Power(false),
    )
    .await
    .expect("the write succeeds")
    .expect("a HID++ light reads back");
    assert!(!off.on);
    let warm = apply(
        &backend,
        &route,
        LitraModel::Glow,
        LightCommand::TemperatureKelvin(2700),
    )
    .await
    .expect("the write succeeds")
    .expect("a HID++ light reads back");
    assert_eq!(warm.temperature_kelvin, Some(2700));

    let light = backend.light();
    assert!(!light.on);
    assert_eq!(light.kelvin, 2700);
    assert!(light.requests.contains(&(1, [0, 0])));
    assert!(light.requests.contains(&(9, 2700u16.to_be_bytes())));
}

#[tokio::test]
async fn the_returned_state_is_what_the_light_reports_not_what_was_sent() {
    let light = Light {
        quirk: Quirk::IgnoresWrites,
        ..Light::glow()
    };
    let backend = LitraBackend::new("stubborn", Opens::Hidpp, light);
    let route = backend.route();

    for command in [
        LightCommand::Power(false),
        LightCommand::BrightnessNative(200),
        LightCommand::TemperatureKelvin(6500),
    ] {
        let state = apply(&backend, &route, LitraModel::Glow, command)
            .await
            .expect("the setter is acknowledged")
            .expect("a HID++ light reads back");
        assert!(state.on, "{command:?}");
        assert_eq!(state.brightness, Some(100), "{command:?}");
        assert_eq!(state.temperature_kelvin, Some(4000), "{command:?}");
    }
    let functions = backend.light().functions();
    for setter in [1, 4, 9] {
        assert!(
            functions.contains(&setter),
            "setter {setter} ran: {functions:?}"
        );
    }
}

#[tokio::test]
async fn a_rejected_hidpp_write_is_returned_and_not_repeated_raw() {
    let light = Light {
        quirk: Quirk::RejectsWrites,
        ..Light::glow()
    };
    let backend = LitraBackend::new("rejects", Opens::Hidpp, light);

    let error = apply(
        &backend,
        &backend.route(),
        LitraModel::Glow,
        LightCommand::Power(false),
    )
    .await
    .expect_err("the light refused the write");
    assert!(
        matches!(
            error,
            WriteError::HidppFeature {
                operation: HidppOperation::Light,
                feature_hex: 0x1990,
                kind: HidppFeatureErrorKind::InvalidArgument,
            }
        ),
        "{error:?}"
    );
    assert!(backend.raw_reports().is_empty(), "nothing goes out raw");
    assert!(backend.light().on);
}

#[tokio::test]
async fn values_are_checked_against_the_ranges_the_light_reports() {
    // A light wider than the Glow's static 20–250 lm range: its own range
    // decides, both ways.
    let light = Light {
        brightness: (10, 300, 10),
        ..Light::glow()
    };
    let backend = LitraBackend::new("range", Opens::Hidpp, light);
    let route = backend.route();

    let state = apply(
        &backend,
        &route,
        LitraModel::Glow,
        LightCommand::BrightnessNative(280),
    )
    .await
    .expect("280 lm is inside the light's own range")
    .expect("a HID++ light reads back");
    assert_eq!(state.brightness, Some(280));

    for (command, control) in [
        (LightCommand::BrightnessNative(285), "brightness_native"),
        (LightCommand::TemperatureKelvin(2750), "temperature_kelvin"),
        (LightCommand::BrightnessPercent(101), "brightness_percent"),
    ] {
        let error = apply(&backend, &route, LitraModel::Glow, command)
            .await
            .expect_err("off the light's grid");
        assert!(
            matches!(&error, WriteError::InvalidLightValue { control: name, .. } if name == control),
            "{error:?}"
        );
    }
    let functions = backend.light().functions();
    assert_eq!(
        functions
            .iter()
            .filter(|function| matches!(function, 4 | 9))
            .count(),
        1,
        "only the valid write ran: {functions:?}"
    );
}

#[tokio::test]
async fn a_light_that_is_not_hidpp_gets_the_raw_report_and_is_not_asked_again() {
    let backend = LitraBackend::new("not-hidpp", Opens::NotHidpp, Light::glow());
    let route = backend.route();

    let state = apply(
        &backend,
        &route,
        LitraModel::Glow,
        LightCommand::TemperatureKelvin(4600),
    )
    .await
    .expect("the raw report goes out");
    assert_eq!(state, None, "a raw report reads nothing back");
    let expected = encode_command(LitraModel::Glow, LightCommand::TemperatureKelvin(4600))
        .expect("valid")
        .to_vec();
    assert_eq!(backend.raw_reports(), [expected]);

    apply(
        &backend,
        &route,
        LitraModel::Glow,
        LightCommand::Power(true),
    )
    .await
    .expect("the raw report goes out");
    assert_eq!(
        read_state(&backend, &route, LitraModel::Glow)
            .await
            .expect("no error"),
        None
    );
    assert_eq!(backend.hidpp_opens(), 1, "the answer is remembered");
    assert_eq!(backend.raw_reports().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_light_that_does_not_answer_falls_back_at_the_resolve_deadline() {
    let backend = LitraBackend::new("silent", Opens::Silent, Light::glow());
    let route = backend.route();

    let started = tokio::time::Instant::now();
    apply(
        &backend,
        &route,
        LitraModel::Glow,
        LightCommand::Power(false),
    )
    .await
    .expect("the raw report goes out");
    let waited = started.elapsed();
    assert!(
        waited >= RESOLVE_TIMEOUT && waited < hidpp::channel::SEND_RESPONSE_TIMEOUT,
        "waited {waited:?}"
    );
    assert_eq!(backend.raw_reports().len(), 1);

    apply(
        &backend,
        &route,
        LitraModel::Glow,
        LightCommand::Power(true),
    )
    .await
    .expect("the raw report goes out");
    assert_eq!(
        backend.hidpp_opens(),
        1,
        "a silent light is not asked again"
    );
    assert_eq!(backend.raw_reports().len(), 2);
}

#[tokio::test]
async fn a_light_without_0x1990_gets_the_raw_report() {
    let light = Light {
        quirk: Quirk::NoIllumination,
        ..Light::glow()
    };
    let backend = LitraBackend::new("absent", Opens::Hidpp, light);
    let route = backend.route();

    for _ in 0..2 {
        apply(
            &backend,
            &route,
            LitraModel::Glow,
            LightCommand::Power(true),
        )
        .await
        .expect("the raw report goes out");
    }
    assert_eq!(backend.raw_reports().len(), 2);
    assert_eq!(backend.hidpp_opens(), 1);
    assert!(backend.light().requests.is_empty());
}

#[tokio::test]
async fn a_light_whose_state_cannot_be_read_gets_the_raw_report() {
    let light = Light {
        quirk: Quirk::RejectsReads,
        ..Light::glow()
    };
    let backend = LitraBackend::new("unreadable", Opens::Hidpp, light);
    let route = backend.route();

    for _ in 0..2 {
        let state = apply(
            &backend,
            &route,
            LitraModel::Glow,
            LightCommand::Power(false),
        )
        .await
        .expect("the raw report goes out");
        assert_eq!(state, None);
    }
    assert_eq!(backend.raw_reports().len(), 2);
    assert_eq!(backend.hidpp_opens(), 1, "the failed read is remembered");
    assert_eq!(
        backend.light().functions(),
        [2],
        "no setter ran over HID++ before falling back"
    );
}

#[tokio::test]
async fn a_refused_open_falls_back_for_that_command_only() {
    let backend = LitraBackend::new("refused", Opens::Fails, Light::glow());
    let route = backend.route();

    for _ in 0..2 {
        apply(
            &backend,
            &route,
            LitraModel::Glow,
            LightCommand::Power(true),
        )
        .await
        .expect("the raw report goes out");
    }
    assert_eq!(backend.raw_reports().len(), 2);
    assert_eq!(
        backend.hidpp_opens(),
        2,
        "a refused open says nothing about the light, so it is asked again"
    );
}

#[tokio::test]
async fn two_nodes_answering_to_one_route_are_refused() {
    let mut backend = LitraBackend::new("twin", Opens::Hidpp, Light::glow());
    let mut twin = litra_node("twin");
    twin.id = NodeId::from("litra-hidpp-test-twin-2".to_owned());
    backend.nodes.push(twin);

    let error = apply(
        &backend,
        &backend.route(),
        LitraModel::Glow,
        LightCommand::Power(true),
    )
    .await
    .expect_err("two nodes match one route");
    assert!(matches!(error, WriteError::AmbiguousRawDevice), "{error:?}");
    assert_eq!(backend.hidpp_opens(), 0);
    assert!(backend.raw_reports().is_empty());
}

#[tokio::test]
async fn another_light_of_the_same_model_is_not_the_routed_one() {
    let mut backend = LitraBackend::new("pair-a", Opens::Hidpp, Light::glow());
    backend.nodes.push(litra_node("pair-b"));

    apply(
        &backend,
        &backend.route(),
        LitraModel::Glow,
        LightCommand::BrightnessNative(120),
    )
    .await
    .expect("the route names one of the two lights by serial");
    assert_eq!(backend.hidpp_opens(), 1);
    assert_eq!(backend.light().lumens, 120);
}

#[tokio::test]
async fn a_disconnected_light_is_not_found() {
    let mut backend = LitraBackend::new("gone", Opens::Hidpp, Light::glow());
    let route = backend.route();
    backend.nodes.clear();

    let error = apply(
        &backend,
        &route,
        LitraModel::Glow,
        LightCommand::Power(true),
    )
    .await
    .expect_err("nothing answers to the route");
    assert!(matches!(error, WriteError::DeviceNotFound), "{error:?}");
}

#[test]
fn a_control_range_keeps_its_top_on_the_resolution_grid() {
    let lumens = LightValueUnit::Lumens;
    let range = control_range(30, 405, 10, lumens).expect("a range");
    assert_eq!((range.min(), range.max(), range.step()), (30, 400, 10));
    let range = control_range(20, 250, 0, lumens).expect("a range");
    assert_eq!(
        (range.max(), range.step()),
        (250, 1),
        "a zero resolution reads as 1"
    );
    assert_eq!(control_range(250, 250, 1, lumens), None, "a single setting");
    assert_eq!(control_range(300, 250, 1, lumens), None, "a reversed range");
}

/// The watcher's next change, failing the test rather than hanging.
/// The agent awaits [`LightWatch::next`] inside `tokio::spawn`, so its
/// future must be `Send`. This fails to compile otherwise.
#[test]
fn a_watch_can_be_awaited_on_another_thread() {
    fn next_is_send(
        watch: &mut LightWatch,
    ) -> impl Future<Output = Option<LightChange>> + Send + '_ {
        watch.next()
    }
    let _ = next_is_send;
}

async fn next_change(watch: &mut LightWatch) -> Option<LightChange> {
    tokio::time::timeout(Duration::from_secs(5), watch.next())
        .await
        .expect("the watcher answers in time")
}

async fn watching(backend: &LitraBackend) -> LightWatch {
    watch(backend, &backend.route(), LitraModel::Glow)
        .await
        .expect("the light answers")
        .expect("a HID++ light can be watched")
}

#[tokio::test]
async fn later_commands_reuse_the_open_link_but_still_read_the_light() {
    // Every command reads the light before it acts, over the one link.
    const READ: [u8; 5] = [2, 7, 0, 3, 8];
    let backend = LitraBackend::new("reuse", Opens::Hidpp, Light::glow());
    let route = backend.route();

    apply(
        &backend,
        &route,
        LitraModel::Glow,
        LightCommand::BrightnessNative(120),
    )
    .await
    .expect("the brightness is written");
    apply(
        &backend,
        &route,
        LitraModel::Glow,
        LightCommand::TemperatureKelvin(5000),
    )
    .await
    .expect("the temperature is written");
    read_state(&backend, &route, LitraModel::Glow)
        .await
        .expect("the light reads back");

    assert_eq!(backend.hidpp_opens(), 1, "the light is opened once");
    assert_eq!(
        backend.light().functions(),
        [&READ[..], &[4, 3], &READ, &[9, 8], &READ].concat()
    );
    let light = backend.light();
    assert_eq!((light.lumens, light.kelvin), (120, 5000));
}

#[tokio::test]
async fn a_link_that_stops_answering_is_opened_again() {
    let backend = LitraBackend::new("gone", Opens::Hidpp, Light::glow());
    let route = backend.route();
    apply(
        &backend,
        &route,
        LitraModel::Glow,
        LightCommand::BrightnessNative(120),
    )
    .await
    .expect("the brightness is written");

    // From here on nothing answers, so the deadlines pass at once.
    tokio::time::pause();
    backend.change_silently(|light| light.quirk = Quirk::Gone);
    let applied = apply(
        &backend,
        &route,
        LitraModel::Glow,
        LightCommand::BrightnessNative(150),
    )
    .await
    .expect("the raw report is sent");

    assert_eq!(
        applied, None,
        "a light that does not answer gets the raw report"
    );
    assert_eq!(backend.hidpp_opens(), 2, "the light is opened again");
    assert_eq!(backend.raw_reports().len(), 1);
}

#[tokio::test]
async fn a_change_made_on_the_light_is_reported() {
    let backend = LitraBackend::new("button", Opens::Hidpp, Light::glow());
    let mut watch = watching(&backend).await;
    assert_eq!(watch.state().brightness, Some(100));

    backend.press(1, |light| {
        light.lumens = 150;
        light.lumens.to_be_bytes().to_vec()
    });

    let change = next_change(&mut watch)
        .await
        .expect("the change is reported");
    assert_eq!(change.state.brightness, Some(150));
    assert_eq!(
        change.on_light,
        LightControls {
            brightness: true,
            ..LightControls::default()
        }
    );
    assert_eq!(watch.state(), change.state);
}

/// Whether the watcher reports nothing for a while. Real time: the scripted
/// light answers from the runtime, and a paused clock could run a deadline out
/// before it does.
async fn stays_quiet(watch: &mut LightWatch) -> bool {
    tokio::time::timeout(Duration::from_millis(200), watch.next())
        .await
        .is_err()
}

/// The watcher's next change made on the light, past the states it reports
/// for OpenLogi's own writes.
async fn next_change_on_light(watch: &mut LightWatch) -> LightChange {
    loop {
        let change = next_change(watch).await.expect("the watcher keeps running");
        if change.on_light != LightControls::default() {
            return change;
        }
    }
}

#[tokio::test]
async fn openlogis_own_write_is_reported_once_and_not_as_a_change_on_the_light() {
    let backend = LitraBackend::new("echo", Opens::Hidpp, Light::glow());
    let route = backend.route();
    let mut watch = watching(&backend).await;

    // The light announces each write it was asked for.
    for command in [
        LightCommand::Power(false),
        LightCommand::BrightnessNative(200),
        LightCommand::TemperatureKelvin(5000),
    ] {
        let applied = apply(&backend, &route, LitraModel::Glow, command)
            .await
            .expect("the command is written")
            .expect("the light reads back");
        let change = next_change(&mut watch)
            .await
            .expect("the write is reported");
        assert_eq!(
            change,
            LightChange {
                state: applied,
                on_light: LightControls::default(),
            },
            "{command:?} is reported with the state it read back"
        );
        assert!(
            stays_quiet(&mut watch).await,
            "the echo of {command:?} is not reported again"
        );
    }
    // Then its power button is pressed.
    backend.press(0, |light| {
        light.on = true;
        vec![1]
    });

    let change = next_change(&mut watch)
        .await
        .expect("the button press is reported");
    assert_eq!(
        change.on_light,
        LightControls {
            power: true,
            ..LightControls::default()
        }
    );
    assert_eq!(change.state.brightness, Some(200));
    assert_eq!(change.state.temperature_kelvin, Some(5000));
}

#[tokio::test]
async fn a_write_the_light_does_not_announce_is_reported_too() {
    let light = Light {
        quirk: Quirk::Unannounced,
        ..Light::glow()
    };
    let backend = LitraBackend::new("unannounced", Opens::Hidpp, light);
    let route = backend.route();
    let mut watch = watching(&backend).await;

    let applied = apply(
        &backend,
        &route,
        LitraModel::Glow,
        LightCommand::BrightnessNative(200),
    )
    .await
    .expect("the brightness is written")
    .expect("the light reads back");

    let change = next_change(&mut watch)
        .await
        .expect("the write is reported");
    assert_eq!(change.state.brightness, Some(200));
    assert_eq!(
        change,
        LightChange {
            state: applied,
            on_light: LightControls::default(),
        }
    );
    assert!(stays_quiet(&mut watch).await, "the write is reported once");
}

#[tokio::test]
async fn a_write_does_not_hide_a_change_made_on_the_light_meanwhile() {
    let backend = LitraBackend::new("meanwhile", Opens::Hidpp, Light::glow());
    let route = backend.route();
    let mut watch = watching(&backend).await;

    // The brightness button is pressed, a temperature write runs before the
    // light's notification arrives, and then the notification arrives.
    backend.change_silently(|light| light.lumens = 150);
    apply(
        &backend,
        &route,
        LitraModel::Glow,
        LightCommand::TemperatureKelvin(5000),
    )
    .await
    .expect("the temperature is written");
    backend.press(1, |light| light.lumens.to_be_bytes().to_vec());

    // The write's own state may be reported first; the press still is.
    let change = next_change_on_light(&mut watch).await;
    assert_eq!(
        change.on_light,
        LightControls {
            brightness: true,
            ..LightControls::default()
        }
    );
    assert_eq!(change.state.brightness, Some(150));
    assert_eq!(change.state.temperature_kelvin, Some(5000));
    assert!(stays_quiet(&mut watch).await, "the press is reported once");
}

#[tokio::test]
async fn a_change_on_the_light_is_reported_even_when_a_write_already_read_it() {
    let light = Light {
        quirk: Quirk::Unannounced,
        ..Light::glow()
    };
    let backend = LitraBackend::new("read-first", Opens::Hidpp, light);
    let route = backend.route();
    let mut watch = watching(&backend).await;

    // The brightness button is pressed, and a temperature write reads the
    // new brightness back before the light's notification arrives.
    backend.change_silently(|light| light.lumens = 150);
    apply(
        &backend,
        &route,
        LitraModel::Glow,
        LightCommand::TemperatureKelvin(5000),
    )
    .await
    .expect("the temperature is written");
    let written = next_change(&mut watch)
        .await
        .expect("the write is reported");
    assert_eq!(written.on_light, LightControls::default());
    assert_eq!(written.state.brightness, Some(150));

    // The light's state is already reported; the press still is.
    backend.press(1, |light| light.lumens.to_be_bytes().to_vec());
    let change = next_change(&mut watch)
        .await
        .expect("the button press is reported");
    assert_eq!(
        change,
        LightChange {
            state: written.state,
            on_light: LightControls {
                brightness: true,
                ..LightControls::default()
            },
        }
    );
}

#[tokio::test]
async fn a_brightness_lowered_to_a_new_limit_is_not_a_change_on_the_light() {
    let mut light = Light::glow();
    light.brightness_flags = DYNAMIC;
    let backend = LitraBackend::new("limit", Opens::Hidpp, light);
    let mut watch = watching(&backend).await;

    backend.press(3, |light| {
        light.effective_max = 80;
        light.lumens = 80;
        80u16.to_be_bytes().to_vec()
    });
    let change = next_change(&mut watch)
        .await
        .expect("the new limit is reported");
    assert_eq!(change.on_light, LightControls::default());
    assert_eq!(change.state.brightness, Some(80));
    assert_eq!(change.state.brightness_limit, Some(80));

    backend.press(4, |light| {
        light.effective_max = 60;
        light.lumens = 60;
        vec![0, 0, 60]
    });
    let change = next_change(&mut watch)
        .await
        .expect("the clamp is reported");
    assert_eq!(change.on_light, LightControls::default());
    assert_eq!(change.state.brightness, Some(60));
}

#[tokio::test]
async fn a_limit_read_behind_another_notification_is_not_a_change_on_the_light() {
    let mut light = Light::glow();
    light.brightness_flags = DYNAMIC;
    let backend = LitraBackend::new("limit-behind", Opens::Hidpp, light);
    let mut watch = watching(&backend).await;

    // The temperature button is pressed, and the light lowers its limit
    // before the temperature notification is read.
    backend.press(2, |light| {
        light.kelvin = 5000;
        5000u16.to_be_bytes().to_vec()
    });
    backend.press(3, |light| {
        light.effective_max = 80;
        light.lumens = 80;
        80u16.to_be_bytes().to_vec()
    });
    let change = next_change(&mut watch)
        .await
        .expect("the temperature press is reported");
    assert_eq!(
        change.on_light,
        LightControls {
            temperature: true,
            ..LightControls::default()
        }
    );
    assert_eq!(change.state.brightness, Some(80));
    assert_eq!(change.state.brightness_limit, Some(80));
    assert!(
        stays_quiet(&mut watch).await,
        "the limit notification read later finds nothing new"
    );
}

#[tokio::test]
async fn a_press_read_with_a_limit_it_stays_under_is_still_a_change_on_the_light() {
    let mut light = Light::glow();
    light.brightness_flags = DYNAMIC;
    let backend = LitraBackend::new("press-under-limit", Opens::Hidpp, light);
    let mut watch = watching(&backend).await;

    // The brightness button takes the light from 100 to 150 lm, and the
    // limit drops to 200 lm before the brightness notification is read:
    // 150 lm is under the new limit, so the limit explains nothing.
    backend.press(1, |light| {
        light.lumens = 150;
        150u16.to_be_bytes().to_vec()
    });
    backend.press(3, |light| {
        light.effective_max = 200;
        200u16.to_be_bytes().to_vec()
    });
    let change = next_change(&mut watch)
        .await
        .expect("the brightness press is reported");
    assert_eq!(
        change.on_light,
        LightControls {
            brightness: true,
            ..LightControls::default()
        }
    );
    assert_eq!(change.state.brightness, Some(150));
    assert_eq!(change.state.brightness_limit, Some(200));
    assert!(
        stays_quiet(&mut watch).await,
        "the limit notification read later finds nothing new"
    );
}

#[tokio::test]
async fn a_press_while_the_light_is_read_is_read_again() {
    let backend = LitraBackend::new("press-during-read", Opens::Hidpp, Light::glow());
    let mut watch = watching(&backend).await;
    // The temperature button wakes the watcher. The brightness button goes
    // to 150 lm after the read passed the brightness.
    backend.press(2, |light| {
        light.kelvin = 5000;
        light.presses_during_reads = vec![150];
        5000u16.to_be_bytes().to_vec()
    });
    let change = next_change(&mut watch)
        .await
        .expect("both presses are reported");
    assert_eq!(
        change.on_light,
        LightControls {
            brightness: true,
            temperature: true,
            ..LightControls::default()
        }
    );
    assert_eq!(change.state.brightness, Some(150));
    assert_eq!(change.state.temperature_kelvin, Some(5000));
    assert!(stays_quiet(&mut watch).await);
}

#[tokio::test]
async fn a_light_that_keeps_notifying_during_reads_is_read_again_next_time() {
    let backend = LitraBackend::new("presses-during-reads", Opens::Hidpp, Light::glow());
    let mut watch = watching(&backend).await;
    // A press lands during each of the first four reads. One wake reads the
    // light three times and reports the brightness its third read found.
    backend.press(2, |light| {
        light.kelvin = 5000;
        light.presses_during_reads = vec![110, 120, 130, 140];
        5000u16.to_be_bytes().to_vec()
    });
    let change = next_change(&mut watch)
        .await
        .expect("the reads so far are reported");
    assert_eq!(change.state.brightness, Some(120));
    // The next call reads without another notification, until no press is
    // waiting.
    let change = next_change(&mut watch)
        .await
        .expect("the presses still waiting are read");
    assert!(change.on_light.brightness);
    assert_eq!(change.state.brightness, Some(140));
    assert!(stays_quiet(&mut watch).await);
}

/// A light at 100 lm under a steady 120 lm limit.
fn under_a_steady_limit() -> Light {
    let mut light = Light::glow();
    light.brightness_flags = DYNAMIC;
    light.effective_max = 120;
    light
}

/// The clamp notification `0x1990` sends, from `source`, at `lumens`.
fn clamp(source: u8, lumens: u16) -> Vec<u8> {
    let mut payload = vec![source];
    payload.extend_from_slice(&lumens.to_be_bytes());
    payload
}

#[tokio::test]
async fn a_brightness_a_request_was_clamped_to_is_not_a_change_on_the_light() {
    for clamp_first in [true, false] {
        let backend = LitraBackend::new(
            &format!("request-clamped-{clamp_first}"),
            Opens::Hidpp,
            under_a_steady_limit(),
        );
        let mut watch = watching(&backend).await;
        // Another program asks for 200 lm: the light holds its 120 lm limit
        // and says the request was clamped, before or after its brightness.
        let brightness = |light: &mut Light| {
            light.lumens = 120;
            120u16.to_be_bytes().to_vec()
        };
        if clamp_first {
            backend.press(4, |_| clamp(1, 120));
            backend.press(1, brightness);
        } else {
            backend.press(1, brightness);
            backend.press(4, |_| clamp(1, 120));
        }
        let change = next_change(&mut watch)
            .await
            .expect("the new brightness is reported");
        assert_eq!(
            change.on_light,
            LightControls::default(),
            "clamp first: {clamp_first}"
        );
        assert_eq!(change.state.brightness, Some(120));
        assert_eq!(change.state.brightness_limit, Some(120));
        assert!(stays_quiet(&mut watch).await, "clamp first: {clamp_first}");
    }
}

#[tokio::test]
async fn a_clamp_the_button_made_still_follows_a_press_on_the_light() {
    let backend = LitraBackend::new("button-clamped", Opens::Hidpp, under_a_steady_limit());
    let mut watch = watching(&backend).await;
    backend.press(1, |light| {
        light.lumens = 120;
        120u16.to_be_bytes().to_vec()
    });
    backend.press(4, |_| clamp(2, 120));
    let change = next_change(&mut watch)
        .await
        .expect("the brightness press is reported");
    assert_eq!(
        change.on_light,
        LightControls {
            brightness: true,
            ..LightControls::default()
        }
    );
    assert_eq!(change.state.brightness, Some(120));
}

#[tokio::test]
async fn a_clamp_does_not_hide_a_later_press_back_to_its_brightness() {
    let backend = LitraBackend::new("clamp-then-presses", Opens::Hidpp, under_a_steady_limit());
    let mut watch = watching(&backend).await;
    // Another program's request is clamped to 120 lm. The button then goes
    // to 105, 110 and 120 lm, one press during each read of the wake.
    backend.press(4, |light| {
        light.lumens = 120;
        light.presses_during_reads = vec![105, 110, 120];
        clamp(1, 120)
    });
    let change = next_change(&mut watch)
        .await
        .expect("the reads so far are reported");
    assert!(change.on_light.brightness);
    assert_eq!(change.state.brightness, Some(110));
    let change = next_change(&mut watch)
        .await
        .expect("the last press is read");
    assert!(
        change.on_light.brightness,
        "the press back to 120 lm is not explained by the old clamp"
    );
    assert_eq!(change.state.brightness, Some(120));
    assert!(stays_quiet(&mut watch).await);
}

#[tokio::test]
async fn a_clamp_the_button_made_drops_an_earlier_clamp() {
    let backend = LitraBackend::new(
        "request-then-button-clamp",
        Opens::Hidpp,
        under_a_steady_limit(),
    );
    let mut watch = watching(&backend).await;
    // A request is clamped to 120 lm, and then the button holds the light
    // at the same limit, before the watcher reads it.
    backend.press(4, |light| {
        light.lumens = 120;
        clamp(1, 120)
    });
    backend.press(4, |_| clamp(2, 120));
    let change = next_change(&mut watch)
        .await
        .expect("the new brightness is reported");
    assert!(change.on_light.brightness, "the button chose the limit");
    assert_eq!(change.state.brightness, Some(120));
    assert!(stays_quiet(&mut watch).await);
}

#[tokio::test]
async fn a_clamp_explains_only_the_brightness_it_clamped_to() {
    let backend = LitraBackend::new(
        "clamp-other-brightness",
        Opens::Hidpp,
        under_a_steady_limit(),
    );
    let mut watch = watching(&backend).await;
    // The light is at 105 lm, a change it did not announce, when a request
    // is clamped to 120 lm.
    backend.change_silently(|light| light.lumens = 105);
    backend.press(4, |_| clamp(1, 120));
    let change = next_change(&mut watch)
        .await
        .expect("the new brightness is reported");
    assert!(
        change.on_light.brightness,
        "105 lm is not the clamped brightness"
    );
    assert_eq!(change.state.brightness, Some(105));
    assert!(stays_quiet(&mut watch).await);
}

#[tokio::test]
async fn a_clamp_explains_only_the_wake_that_took_it() {
    let backend = LitraBackend::new("clamp-one-wake", Opens::Hidpp, under_a_steady_limit());
    let mut watch = watching(&backend).await;
    backend.press(4, |light| {
        light.lumens = 120;
        clamp(1, 120)
    });
    let change = next_change(&mut watch)
        .await
        .expect("the clamped brightness is reported");
    assert_eq!(change.on_light, LightControls::default());
    // Brightness changes the light did not announce, each read after a
    // temperature press.
    for (lumens, kelvin) in [(105u16, 4100u16), (120, 4200)] {
        backend.change_silently(|light| light.lumens = lumens);
        backend.press(2, |light| {
            light.kelvin = kelvin;
            kelvin.to_be_bytes().to_vec()
        });
        let change = next_change(&mut watch)
            .await
            .expect("the press is reported");
        assert!(change.on_light.brightness, "{lumens} lm");
        assert_eq!(change.state.brightness, Some(lumens));
    }
    assert!(stays_quiet(&mut watch).await);
}

#[tokio::test]
async fn a_press_after_a_clamp_is_still_a_change_on_the_light() {
    let backend = LitraBackend::new("pressed-after-clamp", Opens::Hidpp, under_a_steady_limit());
    let mut watch = watching(&backend).await;
    backend.press(4, |light| {
        light.lumens = 120;
        clamp(1, 120)
    });
    backend.press(1, |light| {
        light.lumens = 110;
        110u16.to_be_bytes().to_vec()
    });
    let change = next_change(&mut watch)
        .await
        .expect("the brightness press is reported");
    assert_eq!(
        change.on_light,
        LightControls {
            brightness: true,
            ..LightControls::default()
        }
    );
    assert_eq!(change.state.brightness, Some(110));

    // The clamp explained only the read that followed it.
    backend.press(1, |light| {
        light.lumens = 120;
        120u16.to_be_bytes().to_vec()
    });
    let change = next_change(&mut watch)
        .await
        .expect("the press back to the limit is reported");
    assert!(change.on_light.brightness);
    assert_eq!(change.state.brightness, Some(120));
}

#[tokio::test]
async fn a_brightness_raised_back_from_its_limit_is_not_a_change_on_the_light() {
    let mut light = Light::glow();
    light.brightness_flags = DYNAMIC;
    let backend = LitraBackend::new("limit-released", Opens::Hidpp, light);
    let mut watch = watching(&backend).await;

    backend.press(3, |light| {
        light.effective_max = 80;
        light.lumens = 80;
        80u16.to_be_bytes().to_vec()
    });
    let change = next_change(&mut watch)
        .await
        .expect("the new limit is reported");
    assert_eq!(change.on_light, LightControls::default());

    // Full power is back, and the light raises its brightness from the
    // limit it was held at.
    backend.press(3, |light| {
        light.effective_max = 250;
        light.lumens = 120;
        250u16.to_be_bytes().to_vec()
    });
    let change = next_change(&mut watch)
        .await
        .expect("the lifted limit is reported");
    assert_eq!(change.on_light, LightControls::default());
    assert_eq!(change.state.brightness, Some(120));
    assert_eq!(change.state.brightness_limit, None);
}

#[tokio::test]
async fn a_press_read_with_a_lifted_limit_is_still_a_change_on_the_light() {
    let mut light = Light::glow();
    light.brightness_flags = DYNAMIC;
    let backend = LitraBackend::new("press-limit-lifted", Opens::Hidpp, light);
    let mut watch = watching(&backend).await;

    backend.press(3, |light| {
        light.effective_max = 200;
        200u16.to_be_bytes().to_vec()
    });
    let change = next_change(&mut watch)
        .await
        .expect("the new limit is reported");
    assert_eq!(change.on_light, LightControls::default());
    assert_eq!(change.state.brightness, Some(100));

    // The light sat below its 200 lm limit, so a rise read together with the
    // lifted limit is the brightness button, not the light coming back up.
    backend.press(1, |light| {
        light.lumens = 150;
        150u16.to_be_bytes().to_vec()
    });
    backend.press(3, |light| {
        light.effective_max = 250;
        250u16.to_be_bytes().to_vec()
    });
    let change = next_change(&mut watch)
        .await
        .expect("the brightness press is reported");
    assert_eq!(
        change.on_light,
        LightControls {
            brightness: true,
            ..LightControls::default()
        }
    );
    assert_eq!(change.state.brightness, Some(150));
    assert_eq!(change.state.brightness_limit, None);
    assert!(
        stays_quiet(&mut watch).await,
        "the limit notification read later finds nothing new"
    );
}

#[tokio::test]
async fn a_press_a_new_limit_cuts_short_is_not_kept_as_the_brightness() {
    let mut light = Light::glow();
    light.brightness_flags = DYNAMIC;
    let backend = LitraBackend::new("press-cut-short", Opens::Hidpp, light);
    let mut watch = watching(&backend).await;

    // The brightness button raises the light from 100 lm while a new 120 lm
    // limit stops it there: the light sits at a limit lower than before.
    backend.press(1, |light| {
        light.effective_max = 120;
        light.lumens = 120;
        120u16.to_be_bytes().to_vec()
    });
    let change = next_change(&mut watch)
        .await
        .expect("the new state is reported");
    assert_eq!(change.on_light, LightControls::default());
    assert_eq!(change.state.brightness, Some(120));
    assert_eq!(change.state.brightness_limit, Some(120));
}

#[tokio::test]
async fn a_read_after_a_notification_replaces_an_older_write_reading() {
    let light = Light {
        quirk: Quirk::Unannounced,
        ..Light::glow()
    };
    let backend = LitraBackend::new("superseded", Opens::Hidpp, light);
    let route = backend.route();
    let mut watch = watching(&backend).await;

    // OpenLogi writes 200 lm, then the brightness button lowers it to 150 lm
    // before the watcher takes the write's reading.
    apply(
        &backend,
        &route,
        LitraModel::Glow,
        LightCommand::BrightnessNative(200),
    )
    .await
    .expect("the brightness is written")
    .expect("the light reads back");
    backend.press(1, |light| {
        light.lumens = 150;
        150u16.to_be_bytes().to_vec()
    });

    // Let the press notification reach the watcher before it next looks, so
    // the notification is taken before the write's reading.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut reported = Vec::new();
    while let Ok(change) = tokio::time::timeout(Duration::from_millis(200), watch.next()).await {
        reported.push(change.expect("the link stays"));
    }
    let Some(last) = reported.last() else {
        panic!("the button press is reported");
    };
    assert_eq!(
        *last,
        LightChange {
            state: watch.state(),
            on_light: LightControls {
                brightness: true,
                ..LightControls::default()
            },
        },
        "the press is reported last, not the write's older reading: {reported:?}"
    );
    assert_eq!(watch.state().brightness, Some(150));
}

#[tokio::test]
async fn a_raw_only_light_cannot_be_watched_and_a_refused_open_can_be_retried() {
    let raw_only = LitraBackend::new("watch-raw", Opens::NotHidpp, Light::glow());
    assert!(matches!(
        watch(&raw_only, &raw_only.route(), LitraModel::Glow).await,
        Ok(None)
    ));

    let refused = LitraBackend::new("watch-refused", Opens::Fails, Light::glow());
    assert!(
        watch(&refused, &refused.route(), LitraModel::Glow)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_watcher_ends_when_its_link_is_retired() {
    let backend = LitraBackend::new("watch-gone", Opens::Hidpp, Light::glow());
    let route = backend.route();
    let mut watch = watching(&backend).await;

    // From here on nothing answers, so the deadlines pass at once.
    tokio::time::pause();
    backend.change_silently(|light| light.quirk = Quirk::Gone);
    apply(
        &backend,
        &route,
        LitraModel::Glow,
        LightCommand::BrightnessNative(150),
    )
    .await
    .expect("the raw report is sent");

    assert_eq!(next_change(&mut watch).await, None);
}

#[tokio::test]
async fn a_watcher_ends_when_its_light_comes_back_as_another_node() {
    let before = LitraBackend::new("moved", Opens::Hidpp, Light::glow());
    let mut watch = watching(&before).await;

    // The same light, by serial, under a new OS node.
    let mut after = LitraBackend::new("moved", Opens::Hidpp, Light::glow());
    after.nodes[0].id = NodeId::from("litra-hidpp-test-moved-again".to_string());
    assert_eq!(after.route(), before.route());
    apply(
        &after,
        &after.route(),
        LitraModel::Glow,
        LightCommand::BrightnessNative(150),
    )
    .await
    .expect("the brightness is written over the new node");

    assert_eq!(next_change(&mut watch).await, None);
}
