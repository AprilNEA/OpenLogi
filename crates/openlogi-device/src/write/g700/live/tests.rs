use std::sync::{Arc, Mutex};

use openlogi_core::hid::{Dpi, ReportRate};

use super::{dpi_info, report_rate_info, set_dpi_target, set_report_rate};
use crate::SharedChannel;
use crate::channel::route::DeviceRoute;
use crate::channel::scripted::{ScriptedRawHidChannel, register_error, scripted_channel};
use crate::replay::ReplayChannelHandle;
use crate::write::g700::G700Link;
use crate::write::{DpiTarget, WriteError};

/// The registers of a G700 behind its receiver, as these entry points see
/// them, and its directory and profile pages for the default-DPI lookup.
struct Mouse {
    resolution: [u8; 4],
    interval: u8,
    current_profile: [u8; 3],
    pages: Vec<[u8; 512]>,
    /// Acknowledge resolution writes without applying them.
    ignores_resolution: bool,
}

impl Mouse {
    fn new() -> Self {
        let mut pages = vec![[0xff; 512]; 32];
        // The directory lists one profile, on page 2.
        pages[1][..3].copy_from_slice(&[0x02, 0x00, 0x01]);
        // Its DPI modes: 400 (raw 0x11) and 1600 (raw 0x44); it starts in 1.
        pages[2][..8].copy_from_slice(&[0x11, 0x11, 0x00, 0x00, 0x44, 0x44, 0x00, 0x00]);
        pages[2][0x14] = 1;
        Self {
            resolution: [36, 0, 36, 0],
            interval: 4,
            current_profile: [0x00, 0x00, 0x00],
            pages,
            ignores_resolution: false,
        }
    }

    fn respond(&mut self, request: &[u8]) -> Vec<u8> {
        let (report, device, sub_id, address) = (request[0], request[1], request[2], request[3]);
        let short = |data: [u8; 3]| vec![0x10, device, sub_id, address, data[0], data[1], data[2]];
        let long = |data: &[u8]| {
            let mut reply = vec![0x11, device, sub_id, address];
            reply.extend_from_slice(data);
            reply.resize(20, 0);
            reply
        };
        match (report, sub_id, address) {
            (0x10, 0x83, 0x63) => long(&self.resolution),
            (0x11, 0x82, 0x63) => {
                if !self.ignores_resolution {
                    self.resolution.copy_from_slice(&request[4..8]);
                }
                short([0; 3])
            }
            (0x10, 0x81, 0x64) => short([self.interval, 0, 0]),
            (0x10, 0x80, 0x64) => {
                self.interval = request[4];
                short([0; 3])
            }
            (0x10, 0x81, 0x0f) => short(self.current_profile),
            (0x10, 0x83, 0xa2) => {
                let start = usize::from(request[5]) * 2;
                long(&self.pages[usize::from(request[4])][start..start + 16])
            }
            _ => register_error(request, 0x02),
        }
    }
}

/// A link to `mouse` behind a G700 receiver whose uid is `uid` — unique per
/// test, since the link takes the node's host-wide register lock.
async fn link(mouse: Mouse, uid: &str) -> (G700Link, ReplayChannelHandle, Arc<Mutex<Mouse>>) {
    let mouse = Arc::new(Mutex::new(mouse));
    let responder = Arc::clone(&mouse);
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        Some(responder.lock().unwrap().respond(request))
    });
    let shared = SharedChannel::new(
        scripted_channel(raw.presenting_as(0xc531)).await,
        DeviceRoute::Unifying {
            receiver_uid: format!("live-{uid}-{}", std::process::id()),
            slot: 1,
        },
    );
    let link = G700Link::on_shared(&shared).await.unwrap().unwrap();
    (link, handle, mouse)
}

#[tokio::test]
async fn the_dpi_is_the_live_resolution_over_the_sensor_table() {
    let (link, handle, _) = link(Mouse::new(), "dpi-info").await;

    let info = dpi_info(&link).await.unwrap();

    assert_eq!(info.current, Dpi::new(850));
    let values = info.capabilities.values();
    assert_eq!(values.first(), Some(&Dpi::new(25)));
    assert_eq!(values.last(), Some(&Dpi::new(5700)));
    assert_eq!(values.len(), 228, "every multiple of 25 up to 5700");
    assert_eq!(info.capabilities.step_hint(), Dpi::new(25));
    assert_eq!(
        handle.written_reports(),
        [vec![0x10, 0x01, 0x83, 0x63, 0x00, 0x00, 0x00]],
        "one register read, nothing written"
    );
}

#[tokio::test]
async fn a_dpi_goes_to_the_live_resolution_of_both_axes() {
    let (link, handle, mouse) = link(Mouse::new(), "set-dpi").await;

    let written = set_dpi_target(&link, DpiTarget::Value(Dpi::new(425)))
        .await
        .unwrap();

    assert_eq!(written, Some(Dpi::new(425)));
    let mut set = vec![0x11, 0x01, 0x82, 0x63, 18, 0, 18, 0];
    set.resize(20, 0);
    assert_eq!(
        handle.written_reports(),
        [set, vec![0x10, 0x01, 0x83, 0x63, 0x00, 0x00, 0x00]],
        "a write and its read-back, no flash access"
    );
    assert_eq!(mouse.lock().unwrap().resolution, [18, 0, 18, 0]);
}

#[tokio::test]
async fn a_dpi_off_the_table_is_refused_without_a_frame() {
    let (link, handle, _) = link(Mouse::new(), "off-table").await;

    let result = set_dpi_target(&link, DpiTarget::Value(Dpi::new(860))).await;

    assert!(
        matches!(result, Err(WriteError::DpiUnsupported { requested }) if requested == Dpi::new(860)),
        "{result:?}"
    );
    assert!(handle.written_reports().is_empty());
}

#[tokio::test]
async fn a_resolution_the_mouse_keeps_is_written_twice_then_reported() {
    let mut mouse = Mouse::new();
    mouse.ignores_resolution = true;
    let (link, handle, _) = link(mouse, "not-applied").await;

    let result = set_dpi_target(&link, DpiTarget::Value(Dpi::new(1600))).await;

    assert!(
        matches!(
            result,
            Err(WriteError::DpiNotApplied { requested, actual })
                if requested == Dpi::new(1600) && actual == Dpi::new(850)
        ),
        "{result:?}"
    );
    let writes = handle
        .written_reports()
        .iter()
        .filter(|report| report[2] == 0x82)
        .count();
    assert_eq!(writes, 2);
}

#[tokio::test]
async fn the_default_dpi_is_the_mode_the_played_profile_starts_in() {
    let (link, _, mouse) = link(Mouse::new(), "default").await;

    let written = set_dpi_target(&link, DpiTarget::Default).await.unwrap();

    assert_eq!(written, Some(Dpi::new(1600)));
    assert_eq!(mouse.lock().unwrap().resolution, [0x44, 0, 0x44, 0]);
}

#[tokio::test]
async fn the_factory_profile_has_no_default_dpi_to_write() {
    let mut mouse = Mouse::new();
    mouse.current_profile = [0xff, 0x00, 0x00];
    let (link, handle, _) = link(mouse, "factory").await;

    let written = set_dpi_target(&link, DpiTarget::Default).await.unwrap();

    assert_eq!(written, None);
    assert!(
        handle
            .written_reports()
            .iter()
            .all(|report| report[2] != 0x82),
        "nothing written"
    );
}

#[tokio::test]
async fn the_report_rate_is_the_live_interval_among_four_rates() {
    let (link, _, _) = link(Mouse::new(), "rate-info").await;

    let info = report_rate_info(&link).await.unwrap();

    assert_eq!(info.current, ReportRate::new(250));
    assert_eq!(
        info.supported,
        [125, 250, 500, 1000].map(ReportRate::new),
        "ascending"
    );
}

#[tokio::test]
async fn a_report_rate_goes_to_the_live_interval() {
    let (link, handle, mouse) = link(Mouse::new(), "set-rate").await;

    let info = set_report_rate(&link, ReportRate::new(500)).await.unwrap();

    assert_eq!(info.current, ReportRate::new(500));
    assert_eq!(
        handle.written_reports(),
        [
            vec![0x10, 0x01, 0x81, 0x64, 0x00, 0x00, 0x00],
            vec![0x10, 0x01, 0x80, 0x64, 0x02, 0x00, 0x00],
            vec![0x10, 0x01, 0x81, 0x64, 0x00, 0x00, 0x00],
        ]
    );
    assert_eq!(mouse.lock().unwrap().interval, 2);
}

#[tokio::test]
async fn the_rate_already_set_is_not_written_again() {
    let (link, handle, _) = link(Mouse::new(), "same-rate").await;

    set_report_rate(&link, ReportRate::new(250)).await.unwrap();

    assert!(
        handle
            .written_reports()
            .iter()
            .all(|report| report[2] != 0x80)
    );
}

#[tokio::test]
async fn a_rate_no_interval_gives_is_refused_without_a_frame() {
    let (link, handle, _) = link(Mouse::new(), "bad-rate").await;

    for rate in [333, 2000, 0] {
        let result = set_report_rate(&link, ReportRate::new(rate)).await;
        assert!(
            matches!(result, Err(WriteError::ReportRateUnsupported { .. })),
            "{rate} Hz: {result:?}"
        );
    }
    assert!(handle.written_reports().is_empty());
}
