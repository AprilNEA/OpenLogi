use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use hidpp::channel::HidppChannel;

use super::{
    ActiveSelection, DpiSettings, DpiStage, DpiState, DpiValue, G600ReadError, G600WriteError,
    OnboardProfile, ProfileDpi, StageAfterStore, read_dpi, set_active_stage, set_profile_dpi,
};
use crate::backend::{BackendError, HidBackend, HotplugStream, NodeId, NodeInfo, RawWriter};
use crate::channel::route::DeviceRoute;
use crate::write::WriteError;

/// What the test backend does when report `id` is read.
enum Answer {
    Bytes(Vec<u8>),
    Fails,
    Hangs,
}

/// What the test backend does with a feature-report write.
#[derive(Clone, Copy)]
enum OnWrite {
    /// Apply it the way the mouse does, so a read-back sees it.
    Apply,
    /// Accept it, then store the profile with its last byte changed.
    Corrupt,
    /// Accept it and change nothing.
    Ignore,
    /// Apply a profile write, but ignore a stage selection.
    IgnoreStage,
    /// Accept it, then fail every read of that report.
    Unreadable,
    /// Apply it, then move the mouse to profile `to` the way its profile
    /// button does.
    SwitchProfile {
        /// The 1-based profile the mouse moves to.
        to: u8,
    },
    /// Apply it, and on a stage selection move the mouse to profile `to`
    /// first, so the selection lands there.
    SwitchProfileOnStage {
        /// The 1-based profile the mouse moves to.
        to: u8,
    },
    /// Fail it.
    Fail,
    /// Never complete it.
    Hangs,
}

/// A backend holding one HID node that answers feature reads from a table
/// keyed by the report ID the caller puts in byte 0, and records the ID of
/// every feature read and every feature write.
struct FeatureBackend {
    nodes: Vec<NodeInfo>,
    answers: Mutex<HashMap<u8, Answer>>,
    on_write: OnWrite,
    reads: Mutex<Vec<u8>>,
    writes: Mutex<Vec<Vec<u8>>>,
    /// When each read or write began, in call order.
    requests: Mutex<Vec<tokio::time::Instant>>,
}

impl FeatureBackend {
    fn new(nodes: Vec<NodeInfo>, answers: HashMap<u8, Answer>, on_write: OnWrite) -> Self {
        Self {
            nodes,
            answers: Mutex::new(answers),
            on_write,
            reads: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<tokio::time::Instant> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn record_request(&self) {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(tokio::time::Instant::now());
    }

    fn reads(&self) -> Vec<u8> {
        self.reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn writes(&self) -> Vec<Vec<u8>> {
        self.writes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn report(&self, id: u8) -> Vec<u8> {
        match self
            .answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
        {
            Some(Answer::Bytes(bytes)) => bytes.clone(),
            _ => panic!("report {id:#04x} has no bytes"),
        }
    }
}

#[hidpp::async_trait]
impl HidBackend for FeatureBackend {
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
        Err(BackendError::Backend("a G600 takes no raw writes".into()))
    }

    async fn read_feature_report(
        &self,
        _node: &NodeInfo,
        report: &mut [u8],
    ) -> Result<usize, BackendError> {
        self.record_request();
        self.reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(report[0]);
        // `None` is a hang, awaited only once the lock is released.
        let answer = match self
            .answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&report[0])
        {
            Some(Answer::Bytes(bytes)) => {
                let len = bytes.len().min(report.len());
                report[..len].copy_from_slice(&bytes[..len]);
                Some(Ok(len))
            }
            Some(Answer::Fails) | None => Some(Err(BackendError::Backend("stall".into()))),
            Some(Answer::Hangs) => None,
        };
        match answer {
            Some(answer) => answer,
            None => std::future::pending().await,
        }
    }

    async fn write_feature_report(
        &self,
        _node: &NodeInfo,
        report: &[u8],
    ) -> Result<(), BackendError> {
        self.record_request();
        self.writes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(report.to_vec());
        let stored = match self.on_write {
            OnWrite::Fail => return Err(BackendError::Backend("pipe error".into())),
            OnWrite::Hangs => std::future::pending().await,
            OnWrite::Ignore => return Ok(()),
            OnWrite::IgnoreStage if report[0] == 0xf0 => return Ok(()),
            OnWrite::Unreadable => Answer::Fails,
            OnWrite::SwitchProfileOnStage { to } if report[0] == 0xf0 => {
                let mut active = self.report(0xf0);
                active[1] = ((to - 1) << 4) | (report[1] & 0b110);
                Answer::Bytes(active)
            }
            OnWrite::Apply | OnWrite::SwitchProfile { .. } if report[0] == 0xf0 => {
                // The command keeps the profile and takes the stage bits.
                let mut active = self.report(0xf0);
                active[1] = (active[1] & 0xf0) | (report[1] & 0b110);
                Answer::Bytes(active)
            }
            OnWrite::Apply
            | OnWrite::IgnoreStage
            | OnWrite::SwitchProfile { .. }
            | OnWrite::SwitchProfileOnStage { .. } => Answer::Bytes(report.to_vec()),
            OnWrite::Corrupt => {
                let mut stored = report.to_vec();
                if let Some(last) = stored.last_mut() {
                    *last ^= 0xff;
                }
                Answer::Bytes(stored)
            }
        };
        self.answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(report[0], stored);
        if let OnWrite::SwitchProfile { to } = self.on_write {
            let mut active = self.report(0xf0);
            active[1] = ((to - 1) << 4) | (active[1] & 0x0f);
            self.answers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(0xf0, Answer::Bytes(active));
        }
        Ok(())
    }

    fn watch(&self) -> Result<HotplugStream, BackendError> {
        Ok(Box::new(futures_lite::stream::empty()))
    }
}

fn g600_node() -> NodeInfo {
    NodeInfo {
        id: NodeId::from("g600".to_owned()),
        vendor_id: 0x046d,
        product_id: 0xc24a,
        usage_page: 0xff80,
        usage_id: 0x0080,
        name: "G600 Gaming Mouse".into(),
        manufacturer: Some("Logitech".into()),
        serial_number: None,
    }
}

/// The route inventory gives `node`, through its raw address.
fn route_to(node: &NodeInfo) -> DeviceRoute {
    DeviceRoute::from(&openlogi_core::device::RawDeviceAddress {
        vendor_id: node.vendor_id,
        product_id: node.product_id,
        usage_page: node.usage_page,
        usage_id: node.usage_id,
        identity: node.identity(),
    })
}

fn profile(id: u8, stages: [u8; 4]) -> Vec<u8> {
    let mut report = vec![0; 154];
    report[0] = id;
    report[12] = 0x08; // shift: 400 DPI
    report[13] = 2; // default: second stage
    report[14..18].copy_from_slice(&stages);
    report
}

fn answers() -> HashMap<u8, Answer> {
    HashMap::from([
        // Profile 2, stage 3.
        (0xf0, Answer::Bytes(vec![0xf0, 0x14, 0, 0])),
        (0xf3, Answer::Bytes(profile(0xf3, [0x10, 0x20, 0, 0]))),
        (0xf4, Answer::Bytes(profile(0xf4, [0x18, 0x30, 0x40, 0]))),
        (0xf5, Answer::Bytes(profile(0xf5, [0x08, 0, 0, 0]))),
    ])
}

fn backend(answers: HashMap<u8, Answer>) -> FeatureBackend {
    FeatureBackend::new(vec![g600_node()], answers, OnWrite::Apply)
}

#[tokio::test]
async fn reads_the_active_selection_and_every_profile() {
    let dpi = read_dpi(&backend(answers()), &route_to(&g600_node()))
        .await
        .expect("every report decodes");

    let profile = |stages| ProfileDpi {
        stages,
        default_stage: Some(stage(2)),
        shift: Some(400),
    };
    assert_eq!(
        dpi,
        DpiState {
            active: ActiveSelection {
                profile: profile_number(2),
                dpi_stage: stage(3),
            },
            profiles: [
                profile([Some(800), Some(1600), None, None]),
                profile([Some(1200), Some(2400), Some(3200), None]),
                profile([Some(400), None, None, None]),
            ],
        }
    );
}

#[tokio::test]
async fn a_short_profile_report_is_malformed() {
    let mut answers = answers();
    answers.insert(0xf4, Answer::Bytes(profile(0xf4, [0x10; 4])[..64].to_vec()));

    let error = read_dpi(&backend(answers), &route_to(&g600_node()))
        .await
        .expect_err("a truncated profile must not decode");

    assert!(
        matches!(
            error,
            G600ReadError::Malformed {
                report_id: 0xf4,
                len: 64
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_reply_carrying_another_profiles_id_is_malformed() {
    let mut answers = answers();
    // Asked for profile 2, the mouse answers with profile 1's report.
    answers.insert(0xf4, Answer::Bytes(profile(0xf3, [0x10; 4])));

    let error = read_dpi(&backend(answers), &route_to(&g600_node()))
        .await
        .expect_err("a reply for another profile must not be stored as this one");

    assert!(
        matches!(
            error,
            G600ReadError::Malformed {
                report_id: 0xf4,
                len: 154
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_active_report_naming_a_fourth_profile_is_malformed() {
    let mut answers = answers();
    answers.insert(0xf0, Answer::Bytes(vec![0xf0, 0x30, 0, 0]));

    let error = read_dpi(&backend(answers), &route_to(&g600_node()))
        .await
        .expect_err("profile index 3 does not exist");

    assert!(
        matches!(
            error,
            G600ReadError::Malformed {
                report_id: 0xf0,
                len: 4
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_missing_node_is_device_not_found() {
    let backend = FeatureBackend::new(Vec::new(), answers(), OnWrite::Apply);

    let error = read_dpi(&backend, &route_to(&g600_node()))
        .await
        .expect_err("no node answers the route");

    assert!(
        matches!(error, G600ReadError::Route(WriteError::DeviceNotFound)),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_route_to_another_device_is_refused_before_any_io() {
    let mut node = g600_node();
    node.usage_page = 0x0001;
    node.usage_id = 0x0002;
    let backend = FeatureBackend::new(vec![node.clone()], answers(), OnWrite::Apply);

    let error = read_dpi(&backend, &route_to(&node))
        .await
        .expect_err("the G600's mouse collection carries no feature reports");

    assert!(matches!(error, G600ReadError::NotG600Route), "{error:?}");
    assert!(backend.reads().is_empty(), "{:?}", backend.reads());
}

#[tokio::test]
async fn a_failed_read_carries_the_backend_error() {
    let mut answers = answers();
    answers.insert(0xf5, Answer::Fails);

    let error = read_dpi(&backend(answers), &route_to(&g600_node()))
        .await
        .expect_err("the last profile read fails");

    assert!(
        matches!(error, G600ReadError::Backend(BackendError::Backend(_))),
        "{error:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_read_that_never_completes_times_out() {
    let mut answers = answers();
    answers.insert(0xf3, Answer::Hangs);

    let error = read_dpi(&backend(answers), &route_to(&g600_node()))
        .await
        .expect_err("the first profile read hangs");

    assert!(
        matches!(error, G600ReadError::TimedOut { report_id: 0xf3 }),
        "{error:?}"
    );
}

fn stage(number: u8) -> DpiStage {
    DpiStage::from_number(number).expect("valid stage")
}

fn profile_number(number: u8) -> OnboardProfile {
    OnboardProfile::from_number(number).expect("valid profile")
}

fn dpi(value: u16) -> DpiValue {
    DpiValue::new(value).expect("valid DPI")
}

fn settings(stages: [Option<DpiValue>; 4], default: u8) -> DpiSettings {
    DpiSettings::new(stages, stage(default)).expect("valid settings")
}

/// A profile report with no zero byte, so a byte the write clears shows.
fn busy_profile(id: u8) -> Vec<u8> {
    let mut report: Vec<u8> = (0..154u8).map(|index| index | 0x80).collect();
    report[0] = id;
    report
}

#[tokio::test]
async fn selecting_a_stage_writes_the_command_and_confirms_it() {
    let backend = backend(answers());

    set_active_stage(&backend, &route_to(&g600_node()), stage(1))
        .await
        .expect("the mouse applies the stage");

    assert_eq!(backend.writes(), [vec![0xf0, 0x40, 0, 0]]);
    // The active profile is checked first, the selection read back last.
    assert_eq!(backend.reads(), [0xf0, 0xf4, 0xf0]);
}

#[tokio::test]
async fn a_stage_the_active_profile_disables_is_refused_without_a_write() {
    let backend = backend(answers());

    // Profile 2 is active and its fourth stage holds no DPI.
    let error = set_active_stage(&backend, &route_to(&g600_node()), stage(4))
        .await
        .expect_err("stage 4 is disabled");

    assert!(
        matches!(
            error,
            G600WriteError::StageDisabled { profile, stage: refused }
                if profile == profile_number(2) && refused == stage(4)
        ),
        "{error:?}"
    );
    assert!(backend.writes().is_empty(), "{:?}", backend.writes());
}

#[tokio::test]
async fn a_selected_stage_that_cannot_be_read_back_is_unverified() {
    let backend = FeatureBackend::new(vec![g600_node()], answers(), OnWrite::Unreadable);

    let error = set_active_stage(&backend, &route_to(&g600_node()), stage(1))
        .await
        .expect_err("the read-back fails");

    assert!(
        matches!(
            error,
            G600WriteError::StageUnverified { stage: selected, .. } if selected == stage(1)
        ),
        "{error:?}"
    );
    assert_eq!(backend.writes(), [vec![0xf0, 0x40, 0, 0]]);
}

#[tokio::test]
async fn a_stage_the_mouse_does_not_take_is_a_mismatch() {
    let backend = FeatureBackend::new(vec![g600_node()], answers(), OnWrite::Ignore);

    let error = set_active_stage(&backend, &route_to(&g600_node()), stage(1))
        .await
        .expect_err("the mouse stays on stage 3");

    assert!(
        matches!(
            error,
            G600WriteError::StageMismatch { expected, actual }
                if expected == stage(1) && actual == stage(3)
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn disabling_stages_changes_only_the_stage_and_default_bytes() {
    let mut answers = answers();
    let original = busy_profile(0xf3);
    answers.insert(0xf3, Answer::Bytes(original.clone()));
    let backend = backend(answers);

    set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(1),
        settings([Some(dpi(800)), None, None, Some(dpi(1600))], 4),
        StageAfterStore::Keep,
    )
    .await
    .expect("the profile is written and reads back");

    let writes = backend.writes();
    assert_eq!(
        writes.len(),
        1,
        "an inactive profile needs no stage re-select"
    );
    let written = &writes[0];
    assert_eq!(written.len(), 154);
    assert_eq!(written[13..18], [4, 0x10, 0x00, 0x00, 0x20]);
    for (index, (before, after)) in original.iter().zip(written).enumerate() {
        if !(13..18).contains(&index) {
            assert_eq!(before, after, "byte {index} changed");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn nothing_reaches_the_mouse_while_it_stores_a_profile() {
    let backend = backend(answers());

    set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(2),
        settings([Some(dpi(800)), None, Some(dpi(1600)), None], 1),
        StageAfterStore::Keep,
    )
    .await
    .expect("the active profile is written and its stage re-selected");

    // Read active, read profile, write profile, read profile back, ...
    let requests = backend.requests();
    assert!(requests.len() > 3, "{requests:?}");
    assert!(
        requests[3] - requests[2] >= super::PROFILE_COMMIT_TIME,
        "the read-back came {:?} after the profile write",
        requests[3] - requests[2]
    );
}

#[tokio::test]
async fn a_profile_that_already_holds_the_settings_is_not_written() {
    let backend = backend(answers());

    // Profile 2 already stores 1200, 2400, 3200 with the second stage default.
    set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(2),
        settings([Some(dpi(1200)), Some(dpi(2400)), Some(dpi(3200)), None], 2),
        StageAfterStore::Keep,
    )
    .await
    .expect("nothing needs changing");

    assert!(backend.writes().is_empty(), "{:?}", backend.writes());
}

#[tokio::test]
async fn writing_the_active_profile_reselects_the_current_stage_while_enabled() {
    let backend = backend(answers());

    set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(2),
        settings([Some(dpi(800)), None, Some(dpi(1600)), None], 1),
        StageAfterStore::Keep,
    )
    .await
    .expect("the active profile is written and its stage re-selected");

    let writes = backend.writes();
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert_eq!(writes[0][0], 0xf4);
    // Stage 3 was active before the write and is still enabled.
    assert_eq!(writes[1], [0xf0, 0x44, 0, 0]);
}

#[tokio::test]
async fn writing_the_active_profile_selects_the_default_when_the_current_stage_is_disabled() {
    let backend = backend(answers());

    set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(2),
        settings([Some(dpi(800)), Some(dpi(1600)), None, None], 2),
        StageAfterStore::Keep,
    )
    .await
    .expect("the active profile is written and its default selected");

    let writes = backend.writes();
    assert_eq!(writes.len(), 2, "{writes:?}");
    // Stage 3 was active but is now disabled, so the default stage 2 is taken.
    assert_eq!(writes[1], [0xf0, 0x42, 0, 0]);
}

/// Saving 400 below the stages of the profile in use renumbers them: 3200,
/// the stage the mouse is on, moves from slot 3 to slot 4.
fn renumbered_with_400_below() -> DpiSettings {
    settings(
        [
            Some(dpi(400)),
            Some(dpi(1200)),
            Some(dpi(2400)),
            Some(dpi(3200)),
        ],
        1,
    )
}

#[tokio::test(start_paused = true)]
async fn a_named_stage_is_selected_once_the_stored_profile_reads_back() {
    let backend = backend(answers());
    let patched = {
        let mut report = backend.report(0xf4);
        report[13] = 1;
        report[14..18].copy_from_slice(&[0x08, 0x18, 0x30, 0x40]);
        report
    };

    set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(2),
        renumbered_with_400_below(),
        StageAfterStore::Use(stage(4)),
    )
    .await
    .expect("the profile is stored and the named stage selected");

    // Read active, read profile, write it, read it back, read active again,
    // select stage 4 and read the active stage back: one stage selection, no
    // other.
    assert_eq!(backend.reads(), [0xf0, 0xf4, 0xf4, 0xf0, 0xf0]);
    assert_eq!(backend.writes(), [patched, vec![0xf0, 0x46, 0, 0]]);
    let requests = backend.requests();
    assert_eq!(requests.len(), 7, "{requests:?}");
    assert!(
        requests[3] - requests[2] >= super::PROFILE_COMMIT_TIME,
        "the read-back came {:?} after the profile write",
        requests[3] - requests[2]
    );
    assert_eq!(
        backend.report(0xf0),
        [0xf0, 0x16, 0, 0],
        "profile 2, stage 4"
    );
}

#[tokio::test]
async fn an_unchanged_profile_still_switches_to_a_named_stage_the_mouse_is_not_on() {
    // Profile 2 already stores 1200, 2400, 3200 with the second stage default.
    let unchanged = settings([Some(dpi(1200)), Some(dpi(2400)), Some(dpi(3200)), None], 2);

    let backend = backend(answers());
    set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(2),
        unchanged,
        StageAfterStore::Use(stage(1)),
    )
    .await
    .expect("the named stage is selected");
    assert_eq!(
        backend.writes(),
        [vec![0xf0, 0x40, 0, 0]],
        "no profile write"
    );

    let backend = self::backend(answers());
    set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(2),
        unchanged,
        StageAfterStore::Use(stage(3)),
    )
    .await
    .expect("the mouse is already on stage 3");
    assert!(backend.writes().is_empty(), "{:?}", backend.writes());
}

#[tokio::test]
async fn a_named_stage_the_new_settings_disable_is_refused_before_any_write() {
    let backend = backend(answers());

    let error = set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(2),
        settings([Some(dpi(800)), Some(dpi(1600)), None, None], 1),
        StageAfterStore::Use(stage(3)),
    )
    .await
    .expect_err("stage 3 is disabled in what would be stored");

    assert!(
        matches!(
            error,
            G600WriteError::StageDisabled { profile, stage: refused }
                if profile == profile_number(2) && refused == stage(3)
        ),
        "{error:?}"
    );
    assert!(backend.writes().is_empty(), "{:?}", backend.writes());
}

#[tokio::test]
async fn a_stage_named_for_a_profile_not_in_use_is_refused_before_any_write() {
    let backend = backend(answers());

    let error = set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(1),
        settings([Some(dpi(800)), Some(dpi(1600)), None, None], 1),
        StageAfterStore::Use(stage(1)),
    )
    .await
    .expect_err("the mouse is using profile 2");

    assert!(
        matches!(
            error,
            G600WriteError::ProfileNotInUse { profile, active }
                if profile == profile_number(1) && active == profile_number(2)
        ),
        "{error:?}"
    );
    assert!(backend.writes().is_empty(), "{:?}", backend.writes());
}

#[tokio::test]
async fn a_profile_that_cannot_be_read_back_is_written_but_unverified() {
    let backend = FeatureBackend::new(vec![g600_node()], answers(), OnWrite::Unreadable);

    let error = set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(3),
        settings([Some(dpi(400)), None, None, None], 1),
        StageAfterStore::Keep,
    )
    .await
    .expect_err("the read-back fails");

    assert!(
        matches!(
            error,
            G600WriteError::Unverified {
                profile,
                source: G600ReadError::Backend(BackendError::Backend(_))
            } if profile == profile_number(3)
        ),
        "{error:?}"
    );
    assert!(
        error
            .to_string()
            .contains("profile 3 (report 0xf5) was written but could not be verified"),
        "{error}"
    );
}

#[tokio::test]
async fn a_profile_that_reads_back_differently_is_a_mismatch() {
    let backend = FeatureBackend::new(vec![g600_node()], answers(), OnWrite::Corrupt);

    let error = set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(3),
        settings([Some(dpi(400)), None, None, Some(dpi(8200))], 1),
        StageAfterStore::Keep,
    )
    .await
    .expect_err("the read-back differs in its last byte");

    assert!(
        matches!(error, G600WriteError::ProfileMismatch { profile } if profile == profile_number(3)),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_failed_reselect_after_a_stored_profile_says_the_profile_is_stored() {
    let backend = FeatureBackend::new(vec![g600_node()], answers(), OnWrite::IgnoreStage);

    let error = set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(2),
        // Stage 3 is active and now disabled, so stage 1 is selected.
        settings([Some(dpi(800)), None, None, None], 1),
        StageAfterStore::Keep,
    )
    .await
    .expect_err("the mouse ignores the stage selection");

    let G600WriteError::StageNotReselected {
        profile,
        stage: reselected,
        source,
    } = &error
    else {
        panic!("expected StageNotReselected, got {error:?}");
    };
    assert_eq!((*profile, *reselected), (profile_number(2), stage(1)));
    assert!(
        matches!(**source, G600WriteError::StageMismatch { .. }),
        "{source:?}"
    );
    assert!(error.to_string().contains("is stored"), "{error}");
}

/// The answers with the mouse on stage 3 of profile 1.
fn answers_on_profile_1() -> HashMap<u8, Answer> {
    let mut answers = answers();
    answers.insert(0xf0, Answer::Bytes(vec![0xf0, 0x04, 0, 0]));
    answers
}

#[tokio::test(start_paused = true)]
async fn a_profile_left_while_it_is_stored_gets_no_stage_selection() {
    let backend = FeatureBackend::new(
        vec![g600_node()],
        answers_on_profile_1(),
        OnWrite::SwitchProfile { to: 2 },
    );

    set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(1),
        settings([Some(dpi(800)), Some(dpi(1600)), None, None], 1),
        StageAfterStore::Keep,
    )
    .await
    .expect("the profile is stored; the mouse loads its default on return");

    let writes = backend.writes();
    assert_eq!(writes.len(), 1, "only the profile is written: {writes:?}");
    assert_eq!(writes[0][0], 0xf3);
}

#[tokio::test(start_paused = true)]
async fn a_named_stage_is_not_selected_once_the_mouse_leaves_the_profile() {
    let backend = FeatureBackend::new(
        vec![g600_node()],
        answers_on_profile_1(),
        OnWrite::SwitchProfile { to: 2 },
    );

    let error = set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(1),
        settings([Some(dpi(800)), Some(dpi(1600)), None, None], 1),
        StageAfterStore::Use(stage(2)),
    )
    .await
    .expect_err("the mouse left profile 1 while it stored it");

    let G600WriteError::StageNotReselected {
        profile,
        stage: named,
        source,
    } = &error
    else {
        panic!("expected StageNotReselected, got {error:?}");
    };
    assert_eq!((*profile, *named), (profile_number(1), stage(2)));
    assert!(
        matches!(
            **source,
            G600WriteError::ProfileLeft { profile, active }
                if profile == profile_number(1) && active == profile_number(2)
        ),
        "{source:?}"
    );
    assert!(
        source.to_string().contains("no DPI stage was selected"),
        "{source}"
    );
    let writes = backend.writes();
    assert_eq!(writes.len(), 1, "no stage selection follows: {writes:?}");
    assert_eq!(writes[0][0], 0xf3);
}

#[tokio::test(start_paused = true)]
async fn a_stage_selection_that_lands_on_another_profile_says_it_was_sent() {
    let backend = FeatureBackend::new(
        vec![g600_node()],
        answers_on_profile_1(),
        OnWrite::SwitchProfileOnStage { to: 2 },
    );

    let error = set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(1),
        settings([Some(dpi(800)), Some(dpi(1600)), None, None], 1),
        StageAfterStore::Use(stage(2)),
    )
    .await
    .expect_err("the mouse moved to profile 2 as the stage was selected");

    let G600WriteError::StageNotReselected {
        profile,
        stage: named,
        source,
    } = &error
    else {
        panic!("expected StageNotReselected, got {error:?}");
    };
    assert_eq!((*profile, *named), (profile_number(1), stage(2)));
    assert!(
        matches!(
            **source,
            G600WriteError::ProfileSwitched { expected, active }
                if expected == profile_number(1) && active == profile_number(2)
        ),
        "{source:?}"
    );
    let writes = backend.writes();
    assert_eq!(writes.len(), 2, "the stage selection went out: {writes:?}");
    assert_eq!((writes[0][0], writes[1][0]), (0xf3, 0xf0));
}

#[tokio::test]
async fn a_stage_that_lands_after_a_profile_switch_is_reported() {
    let backend = FeatureBackend::new(
        vec![g600_node()],
        answers(),
        OnWrite::SwitchProfile { to: 3 },
    );

    let error = set_active_stage(&backend, &route_to(&g600_node()), stage(1))
        .await
        .expect_err("the mouse moved to profile 3");

    assert!(
        matches!(
            error,
            G600WriteError::ProfileSwitched { expected, active }
                if expected == profile_number(2) && active == profile_number(3)
        ),
        "{error:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_write_carries_the_backend_error() {
    let backend = FeatureBackend::new(vec![g600_node()], answers(), OnWrite::Fail);

    let error = set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(1),
        settings([Some(dpi(400)), None, None, None], 1),
        StageAfterStore::Keep,
    )
    .await
    .expect_err("the write fails");

    assert!(
        matches!(
            error,
            G600WriteError::Backend {
                report_id: 0xf3,
                source: BackendError::Backend(_)
            }
        ),
        "{error:?}"
    );
    let writes = backend.writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(writes[0][0], 0xf3, "no stage selection follows a failure");
}

#[tokio::test(start_paused = true)]
async fn a_profile_write_that_never_completes_times_out() {
    let backend = FeatureBackend::new(vec![g600_node()], answers(), OnWrite::Hangs);

    let error = set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(1),
        settings([Some(dpi(400)), None, None, None], 1),
        StageAfterStore::Keep,
    )
    .await
    .expect_err("the write hangs");

    assert!(
        matches!(error, G600WriteError::TimedOut { report_id: 0xf3 }),
        "{error:?}"
    );
}

/// A failed or timed-out profile write may still have reached the mouse, so
/// the call returns no sooner than a stored one would.
#[tokio::test(start_paused = true)]
async fn a_profile_write_that_may_have_landed_still_waits_out_the_commit() {
    for on_write in [OnWrite::Fail, OnWrite::Hangs] {
        let backend = FeatureBackend::new(vec![g600_node()], answers(), on_write);

        set_profile_dpi(
            &backend,
            &route_to(&g600_node()),
            profile_number(1),
            settings([Some(dpi(400)), None, None, None], 1),
            StageAfterStore::Keep,
        )
        .await
        .expect_err("the write does not complete");

        let requests = backend.requests();
        // Read active, read profile, write profile: nothing after it.
        assert_eq!(requests.len(), 3, "{requests:?}");
        let waited = tokio::time::Instant::now() - requests[2];
        assert!(
            waited >= super::PROFILE_COMMIT_TIME,
            "returned {waited:?} after the profile write"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn the_slowest_profile_write_fits_the_bound_clients_wait_for() {
    use openlogi_device_registry::g600::{MAX_PROFILE_WRITE_TIME, REPORT_TIMEOUT};

    // Writing the active profile moves the most reports: it reselects a stage.
    let backend = backend(answers());
    set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(2),
        settings([Some(dpi(800)), None, Some(dpi(1600)), None], 1),
        StageAfterStore::Keep,
    )
    .await
    .expect("the active profile is written and its stage re-selected");

    let reports = u32::try_from(backend.requests().len()).expect("a handful of reports");
    assert!(
        REPORT_TIMEOUT * reports + super::PROFILE_COMMIT_TIME <= MAX_PROFILE_WRITE_TIME,
        "{reports} reports at {REPORT_TIMEOUT:?} each exceed {MAX_PROFILE_WRITE_TIME:?}"
    );
}

#[tokio::test]
async fn a_malformed_profile_is_not_written() {
    let mut answers = answers();
    answers.insert(0xf4, Answer::Bytes(profile(0xf4, [0x10; 4])[..64].to_vec()));
    let backend = backend(answers);

    let error = set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(2),
        settings([Some(dpi(400)), None, None, None], 1),
        StageAfterStore::Keep,
    )
    .await
    .expect_err("a truncated profile cannot be patched");

    assert!(
        matches!(
            error,
            G600WriteError::Read(G600ReadError::Malformed {
                report_id: 0xf4,
                len: 64
            })
        ),
        "{error:?}"
    );
    assert!(backend.writes().is_empty(), "{:?}", backend.writes());
}

#[tokio::test]
async fn writes_to_a_route_to_another_device_are_refused_before_any_io() {
    let mut node = g600_node();
    node.usage_page = 0x0001;
    node.usage_id = 0x0002;
    let backend = FeatureBackend::new(vec![node.clone()], answers(), OnWrite::Apply);

    let stage_error = set_active_stage(&backend, &route_to(&node), stage(1))
        .await
        .expect_err("not the vendor collection");
    let dpi_error = set_profile_dpi(
        &backend,
        &route_to(&node),
        profile_number(1),
        settings([Some(dpi(400)), None, None, None], 1),
        StageAfterStore::Keep,
    )
    .await
    .expect_err("not the vendor collection");

    for error in [stage_error, dpi_error] {
        assert!(
            matches!(error, G600WriteError::Read(G600ReadError::NotG600Route)),
            "{error:?}"
        );
    }
    assert!(backend.reads().is_empty(), "{:?}", backend.reads());
    assert!(backend.writes().is_empty(), "{:?}", backend.writes());
}

#[tokio::test(start_paused = true)]
async fn a_failed_report_write_crosses_ipc_still_warning_it_may_have_landed() {
    let backend = FeatureBackend::new(vec![g600_node()], answers(), OnWrite::Fail);

    let error = set_profile_dpi(
        &backend,
        &route_to(&g600_node()),
        profile_number(1),
        settings([Some(dpi(400)), None, None, None], 1),
        StageAfterStore::Keep,
    )
    .await
    .expect_err("the write fails");

    let WriteError::Hid(text) = WriteError::from(error) else {
        panic!("a backend failure crosses as transport text");
    };
    assert!(text.contains("may still have applied it"), "{text}");
}

#[test]
fn timeouts_cross_ipc_naming_the_report_being_moved() {
    use crate::write::HidppOperation;

    let timed_out = |error: WriteError| match error {
        WriteError::RequestTimedOut { operation } => operation,
        other => panic!("expected a timeout, got {other:?}"),
    };
    assert_eq!(
        timed_out(G600ReadError::TimedOut { report_id: 0xf3 }.into()),
        HidppOperation::ReadG600Dpi
    );
    assert_eq!(
        timed_out(G600WriteError::TimedOut { report_id: 0xf4 }.into()),
        HidppOperation::WriteG600Profile
    );
    assert_eq!(
        timed_out(G600WriteError::TimedOut { report_id: 0xf0 }.into()),
        HidppOperation::WriteG600Stage
    );
}

#[test]
fn g600_failures_cross_ipc_with_their_meaning() {
    use openlogi_core::hid::g600::G600Failure;

    let reselect = G600WriteError::StageNotReselected {
        profile: profile_number(2),
        stage: stage(1),
        source: Box::new(G600WriteError::StageMismatch {
            expected: stage(1),
            actual: stage(2),
        }),
    };
    assert_eq!(
        WriteError::from(reselect),
        WriteError::G600(G600Failure::StageNotReselected {
            profile: profile_number(2),
            stage: stage(1)
        })
    );
    assert_eq!(
        WriteError::from(G600WriteError::Read(G600ReadError::NotG600Route)),
        WriteError::G600(G600Failure::NotG600Route)
    );
    assert_eq!(
        WriteError::from(G600ReadError::Route(WriteError::AmbiguousRawDevice)),
        WriteError::AmbiguousRawDevice
    );
    assert_eq!(
        WriteError::from(G600WriteError::StageDisabled {
            profile: profile_number(2),
            stage: stage(4),
        }),
        WriteError::G600(G600Failure::StageDisabled {
            profile: profile_number(2),
            stage: stage(4)
        })
    );
    assert_eq!(
        WriteError::from(G600WriteError::ProfileNotInUse {
            profile: profile_number(1),
            active: profile_number(2),
        }),
        WriteError::G600(G600Failure::ProfileNotInUse {
            profile: profile_number(1),
            active: profile_number(2)
        })
    );
    assert_eq!(
        WriteError::from(G600WriteError::ProfileSwitched {
            expected: profile_number(2),
            active: profile_number(3),
        }),
        WriteError::G600(G600Failure::ProfileSwitched {
            expected: profile_number(2),
            active: profile_number(3)
        })
    );
    assert_eq!(
        WriteError::from(G600WriteError::ProfileLeft {
            profile: profile_number(1),
            active: profile_number(3),
        }),
        WriteError::G600(G600Failure::ProfileSwitched {
            expected: profile_number(1),
            active: profile_number(3)
        })
    );
}
