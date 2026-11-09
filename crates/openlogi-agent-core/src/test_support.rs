//! Scripted devices shared by the crate's unit tests.

use std::sync::{Arc, Mutex};

use openlogi_hid::{ChannelRegistry, DeviceRoute};

/// The HID++ `0x0003` unit ID a scripted device reports unless a test
/// changes it.
pub(crate) const UNIT: [u8; 4] = [0x12, 0x34, 0x56, 0x78];

/// `responder`, also answering HID++ `0x0003` `getDeviceInfo` with the unit
/// ID `unit` holds when asked, at a feature index no scripted device here
/// uses.
pub(crate) fn reporting_unit(
    unit: Arc<Mutex<[u8; 4]>>,
    responder: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
) -> impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static {
    const DEVICE_INFORMATION: u8 = 0x0e;
    move |request| {
        let call = (request.get(2).copied()?, request.get(3)? >> 4);
        if call == (0, 0) && request.get(4..6)? == [0x00, 0x03] {
            let mut response = vec![0; 20];
            response[..4].copy_from_slice(&request[..4]);
            response[0] = 0x11;
            response[4] = DEVICE_INFORMATION;
            return Some(response);
        }
        if call == (DEVICE_INFORMATION, 0) {
            // `getDeviceInfo`: one entity, then the unit ID.
            let mut response = vec![0; 20];
            response[..4].copy_from_slice(&request[..4]);
            response[0] = 0x11;
            response[4] = 1;
            response[5..9].copy_from_slice(&*unit.lock().unwrap());
            return Some(response);
        }
        responder(request)
    }
}

/// A `0x2201` mouse at `route` in `registry` with one sensor that lists 800
/// and 1600 DPI and keeps the last DPI written. `accept` runs on each DPI
/// write and says whether the mouse takes it. Answers the [`UNIT`] the
/// returned cell holds through `0x0003`; a test changes it to stand another
/// device in on the route.
pub(crate) async fn publish_dpi_mouse(
    registry: &ChannelRegistry,
    route: DeviceRoute,
    accept: impl Fn() -> bool + Send + Sync + 'static,
) -> Arc<Mutex<[u8; 4]>> {
    const DPI: u8 = 4;
    const INVALID_ARGUMENT: u8 = 2;
    let unit = Arc::new(Mutex::new(UNIT));
    let current = Mutex::new(800u16);
    openlogi_device::test_support::publish_scripted_channel(
        registry,
        "dpi-mouse",
        route,
        reporting_unit(Arc::clone(&unit), move |request| {
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
        }),
    )
    .await;
    unit
}
