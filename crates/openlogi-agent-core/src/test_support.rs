//! Scripted devices shared by the crate's unit tests.

use openlogi_hid::{ChannelRegistry, DeviceRoute};

/// A `0x2201` mouse at `route` in `registry` with one sensor that lists 800
/// and 1600 DPI and keeps the last DPI written. `accept` runs on each DPI
/// write and says whether the mouse takes it.
pub(crate) async fn publish_dpi_mouse(
    registry: &ChannelRegistry,
    route: DeviceRoute,
    accept: impl Fn() -> bool + Send + Sync + 'static,
) {
    const DPI: u8 = 4;
    const INVALID_ARGUMENT: u8 = 2;
    let current = std::sync::Mutex::new(800u16);
    openlogi_device::test_support::publish_scripted_channel(
        registry,
        "dpi-mouse",
        route,
        move |request| {
            let mut response = vec![0; 20];
            response[..4].copy_from_slice(request.get(..4)?);
            response[0] = 0x11;
            let mut current = current.lock().unwrap();
            match (request[2], request[3] >> 4) {
                (0, 1) => response[4] = 4,
                (0, 0) => {
                    response[4] = if request[4..6] == [0x22, 0x01] {
                        DPI
                    } else {
                        0
                    }
                }
                (DPI, 0) => response[4] = 1,
                (DPI, 1) => response[5..9].copy_from_slice(&[0x03, 0x20, 0x06, 0x40]),
                (DPI, 2) => response[5..7].copy_from_slice(&current.to_be_bytes()),
                (DPI, 3) if accept() => {
                    *current = u16::from_be_bytes([request[5], request[6]]);
                }
                (DPI, 3) => {
                    response[2..6].copy_from_slice(&[0xff, DPI, request[3], INVALID_ARGUMENT]);
                }
                _ => return None,
            }
            Some(response)
        },
    )
    .await;
}
