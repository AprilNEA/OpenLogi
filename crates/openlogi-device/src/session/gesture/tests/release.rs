//! Arming releases the `0x1b04` diversions no session owns: the write behind
//! it, `release_change`, and the arm-time pass over a scripted mouse's table.

use std::time::Duration;

use super::*;
use crate::channel::scripted::{ScriptedControl, reporting_writes, scripted_mouse};
use crate::replay::ReplayChannelHandle;
use crate::session::capture_restore::{CaptureSessionFailure, release_change};

const DIVERTABLE: u16 = reprog_controls::CidFlags::DIVERTABLE.bits();
const RAW_XY: u16 = reprog_controls::CidFlags::RAW_XY.bits();
const FORCE_RAW_XY: u16 = reprog_controls::CidFlags::FORCE_RAW_XY.bits();

fn control(cid: u16, flags: u16) -> reprog_controls::CtrlIdInfo {
    reprog_controls::CtrlIdInfo {
        cid,
        task_id: 0,
        flags,
    }
}

/// The volatile bits only: `diverted`, `raw_xy`, `force_raw_xy`, nothing else
/// set, no remap.
fn volatile(diverted: bool, raw_xy: bool, force_raw_xy: bool) -> reprog_controls::CidReporting {
    reprog_controls::CidReporting {
        cid: reprog_controls::ControlId(reprog_controls::GESTURE_BUTTON_CID),
        diverted,
        persistently_diverted: false,
        force_raw_xy,
        raw_xy,
        remap: None,
        analytics_key_events: false,
        raw_wheel: false,
    }
}

/// Every stale state a dead process can leave (diverted with or without
/// raw-XY, raw-XY alone) lands on native with both valid bits set; the write
/// is skipped only when the control is already there.
#[test]
fn release_clears_whatever_diversion_it_finds() {
    let gesture = control(reprog_controls::GESTURE_BUTTON_CID, DIVERTABLE | RAW_XY);

    for (found, why) in [
        (volatile(true, true, false), "raw-XY diverted"),
        (volatile(true, false, false), "plainly diverted"),
        (volatile(false, true, false), "raw-XY without divert"),
    ] {
        let change = release_change(gesture, found)
            .unwrap_or_else(|| panic!("{why} must be written back to native"));
        assert_eq!(change.diverted, Some(false), "{why}");
        assert_eq!(change.raw_xy, Some(false), "{why}");
        assert_eq!(change.force_raw_xy, None, "{why}: force raw-XY was clear");
    }
    assert_eq!(
        release_change(gesture, volatile(false, false, false)),
        None,
        "a native control needs no write"
    );
}

/// `forceRawXY` is the bit that takes the *pointer* with it, and nothing in
/// OpenLogi ever sets it — so a set bit is released on its own, with no other
/// diversion present. But only where the control advertises the capability:
/// firmware without it must never see the valid bit, so there the bit is
/// ignored and an otherwise-native control stays unwritten.
#[test]
fn release_clears_force_raw_xy_only_where_the_control_advertises_it() {
    let found = volatile(false, false, true);

    let with = control(
        reprog_controls::control_ids::VIRTUAL_GESTURE_BUTTON.0,
        DIVERTABLE | RAW_XY | FORCE_RAW_XY,
    );
    let change = release_change(with, found).expect("a stray force raw-XY must be cleared");
    assert_eq!(change.force_raw_xy, Some(false));
    assert_eq!((change.diverted, change.raw_xy), (Some(false), Some(false)));

    let without = control(reprog_controls::GESTURE_BUTTON_CID, DIVERTABLE | RAW_XY);
    assert_eq!(
        release_change(without, found),
        None,
        "no capability: the bit is not ours to write"
    );
}

/// Release owns the volatile diversion bits and nothing more: the remap
/// target is carried (every `setCidReporting` rewrites it), the persistent,
/// analytics, and raw-wheel bits are left as found.
#[test]
fn release_carries_the_remap_and_leaves_the_other_bits_alone() {
    let remap = reprog_controls::ControlId(0x0053);
    let gesture = control(reprog_controls::GESTURE_BUTTON_CID, DIVERTABLE | RAW_XY);

    let change =
        release_change(gesture, reporting(true, Some(remap))).expect("diverted must be released");

    assert_eq!(change.remap, Some(remap));
    assert_eq!(change.persistently_diverted, None);
    assert_eq!(change.analytics_key_events, None);
    assert_eq!(change.raw_wheel, None);
    assert_eq!(
        change.force_raw_xy, None,
        "the noisy snapshot has the bit set but the control lacks the capability"
    );
}

const GESTURE_BUTTON: ScriptedControl = ScriptedControl {
    cid: reprog_controls::GESTURE_BUTTON_CID,
    task_id: 0,
    flags: DIVERTABLE | RAW_XY,
    reporting: 0x00,
};
/// Back, left raw-XY diverted by a process that never tore down.
const STALE_BACK: ScriptedControl = ScriptedControl {
    cid: 0x0053,
    task_id: 0,
    flags: DIVERTABLE,
    reporting: 0x11,
};
/// The virtual gesture control, diverted with raw-XY *and* force raw-XY —
/// the state that takes the pointer with it.
const STALE_VIRTUAL: ScriptedControl = ScriptedControl {
    cid: reprog_controls::control_ids::VIRTUAL_GESTURE_BUTTON.0,
    task_id: reprog_controls::task_ids::VIRTUAL_GESTURE_BUTTON.0,
    flags: DIVERTABLE | RAW_XY | FORCE_RAW_XY,
    reporting: 0x51,
};
/// A host key, diverted — the host-switch session's to restore, not ours.
const HOST_KEY: ScriptedControl = ScriptedControl {
    cid: reprog_controls::control_ids::HOST_SWITCH_CHANNEL_1.0,
    task_id: reprog_controls::task_ids::HOST_SWITCH_CHANNEL_1.0,
    flags: DIVERTABLE,
    reporting: 0x01,
};
const NATIVE_MIDDLE: ScriptedControl = ScriptedControl {
    cid: 0x0052,
    task_id: 0,
    flags: DIVERTABLE,
    reporting: 0x00,
};

/// Arm `spec` on a scripted mouse at `route` whose `0x1b04` lists `controls`,
/// through the same entry point a session uses.
async fn arm_scripted(
    controls: Vec<ScriptedControl>,
    reject: Option<u16>,
    route: DeviceRoute,
    spec: &CaptureSpec,
) -> (
    Result<ArmedControls, CaptureSessionFailure>,
    ReplayChannelHandle,
) {
    let (raw, handle) =
        ScriptedRawHidChannel::with_dynamic_responder(scripted_mouse(controls, reject));
    let channel = scripted_channel(raw).await;
    let shared = SharedChannel::new(channel, route);
    let armed = tokio::time::timeout(
        Duration::from_secs(2),
        arm_controls(&shared, spec, &ChannelRegistry::default()),
    )
    .await
    .expect("arming a scripted mouse must not hang");
    (armed, handle)
}

fn gesture_spec() -> CaptureSpec {
    CaptureSpec {
        divert_gesture_sources: vec![reprog_controls::GESTURE_BUTTON_CID],
        ..CaptureSpec::default()
    }
}

/// Arming diverts the spec's control, then releases every other divertable
/// control it finds diverted — including the stale force-raw-XY diversion on
/// a virtual control nothing in OpenLogi models — and leaves a host key to
/// the host-switch session. Already-native controls cost no write, and only
/// the spec's control is the session's to restore.
#[tokio::test]
async fn arming_diverts_the_spec_then_releases_the_rest_of_the_table() {
    let (armed, handle) = arm_scripted(
        vec![
            GESTURE_BUTTON,
            STALE_BACK,
            STALE_VIRTUAL,
            HOST_KEY,
            NATIVE_MIDDLE,
        ],
        None,
        TEST_ROUTE,
        &gesture_spec(),
    )
    .await;

    let armed = armed.unwrap_or_else(|failure| panic!("arming failed: {failure:?}"));
    assert_eq!(
        armed.gesture_cids,
        vec![reprog_controls::GESTURE_BUTTON_CID]
    );
    assert!(armed.button_cids.is_empty() && armed.dpi_cids.is_empty());
    let (_, reporting) = armed
        .armed_reporting()
        .expect("the gesture source was armed");
    assert_eq!(
        reporting.len(),
        1,
        "only the spec's control is the session's to restore"
    );
    assert_eq!(
        reporting_writes(&handle),
        vec![
            // divert=1 dvalid, rawXY=1 rvalid
            (reprog_controls::GESTURE_BUTTON_CID, 0x33),
            // divert=0 dvalid, rawXY=0 rvalid
            (STALE_BACK.cid, 0x22),
            // … plus forceRawXY=0 fvalid, since this control advertises it
            (STALE_VIRTUAL.cid, 0xa2),
        ]
    );
}

/// A spec control a dead process left diverted — here with force raw-XY on
/// top — is written once, to the spec's state with the force bit cleared in
/// the same write, and the found state is recorded as what restore replaces.
#[tokio::test]
async fn arming_rewrites_a_spec_control_found_diverted_and_clears_its_force_raw_xy() {
    let leaked = ScriptedControl {
        flags: DIVERTABLE | RAW_XY | FORCE_RAW_XY,
        reporting: 0x51,
        ..GESTURE_BUTTON
    };

    let (armed, handle) = arm_scripted(
        vec![leaked, NATIVE_MIDDLE],
        None,
        TEST_ROUTE,
        &gesture_spec(),
    )
    .await;

    let armed = armed.unwrap_or_else(|failure| panic!("arming failed: {failure:?}"));
    let (_, reporting) = armed
        .armed_reporting()
        .expect("the gesture source was armed");
    assert_eq!(reporting.len(), 1);
    assert!(reporting[0].original.diverted && reporting[0].original.force_raw_xy);
    assert_eq!(
        reporting_writes(&handle),
        // divert=1 dvalid, rawXY=1 rvalid, forceRawXY=0 fvalid
        vec![(reprog_controls::GESTURE_BUTTON_CID, 0xb3)]
    );
}

/// A control a sibling session on the same device arms — the keyboard
/// session's bound key — is neither diverted nor released here, whatever it
/// is found in.
#[tokio::test]
async fn arming_leaves_a_sibling_sessions_control_as_found() {
    let spec = CaptureSpec {
        sibling_controls: BTreeSet::from([STALE_BACK.cid]),
        ..gesture_spec()
    };

    let (armed, handle) =
        arm_scripted(vec![GESTURE_BUTTON, STALE_BACK], None, TEST_ROUTE, &spec).await;

    armed.unwrap_or_else(|failure| panic!("arming failed: {failure:?}"));
    assert_eq!(
        reporting_writes(&handle),
        vec![(reprog_controls::GESTURE_BUTTON_CID, 0x33)]
    );
}

/// Linux 7.3+ diverts the Bluetooth Signature M650's Back itself and listens
/// to it (#2163), so that diversion is the kernel's, not a leak to release;
/// elsewhere the same bytes are a leak like any other.
#[tokio::test]
async fn arming_keeps_the_kernels_m650_back_diversion() {
    let kernel_back = ScriptedControl {
        reporting: 0x01,
        ..STALE_BACK
    };
    let m650 = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb02a,
    };

    let (armed, handle) = arm_scripted(
        vec![GESTURE_BUTTON, kernel_back],
        None,
        m650,
        &gesture_spec(),
    )
    .await;

    armed.unwrap_or_else(|failure| panic!("arming failed: {failure:?}"));
    let mut expected = vec![(reprog_controls::GESTURE_BUTTON_CID, 0x33)];
    if !cfg!(target_os = "linux") {
        expected.push((STALE_BACK.cid, 0x22));
    }
    assert_eq!(reporting_writes(&handle), expected);
}

/// Release is best effort: a control the session does not capture must not
/// be able to cost it the controls it does. A rejected native write on the
/// stale Back is logged and arming completes with the gesture armed.
#[tokio::test]
async fn a_rejected_release_does_not_cost_the_session_its_own_controls() {
    let (armed, handle) = arm_scripted(
        vec![STALE_BACK, GESTURE_BUTTON],
        Some(STALE_BACK.cid),
        TEST_ROUTE,
        &gesture_spec(),
    )
    .await;

    let armed = armed.unwrap_or_else(|failure| panic!("arming failed: {failure:?}"));
    assert_eq!(
        armed.gesture_cids,
        vec![reprog_controls::GESTURE_BUTTON_CID]
    );
    assert_eq!(
        reporting_writes(&handle),
        vec![
            (reprog_controls::GESTURE_BUTTON_CID, 0x33),
            (STALE_BACK.cid, 0x22),
        ],
        "the spec's control was armed, then the release was attempted"
    );
}

/// The inverse must still hold: a rejected write on a control the spec *does*
/// want fails arming, so the caller rolls back rather than running half-armed.
#[tokio::test]
async fn a_rejected_spec_divert_still_fails_arming() {
    let (armed, _) = arm_scripted(
        vec![GESTURE_BUTTON],
        Some(reprog_controls::GESTURE_BUTTON_CID),
        TEST_ROUTE,
        &gesture_spec(),
    )
    .await;

    assert!(
        armed.is_err(),
        "a spec divert that fails is fatal to arming"
    );
}
