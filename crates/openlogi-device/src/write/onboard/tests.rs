//! Tests for the `0x8100` startup-DPI write against a scripted PRO X
//! SUPERLIGHT.

use std::assert_matches;
use std::sync::{Arc, Mutex};

use super::*;
use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};

/// The feature indices a PRO X SUPERLIGHT reports.
const DEVICE_INFO_INDEX: u8 = 0x02;
const ONBOARD_INDEX: u8 = 0x08;
const DPI_INDEX: u8 = 0x0b;
/// HID++ 2.0 `InvalidArgument`.
const INVALID_ARGUMENT: u8 = 0x02;
/// The receiver slot the scripted mouse answers on.
const SLOT: u8 = 1;
const UNIT: [u8; 4] = [0x12, 0x34, 0x56, 0x78];
/// Profile 1 as read from the hardware: slots 400/800/1600/3200/4950,
/// starting in slot 4.
const PROFILE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../openlogi-hidpp/src/feature/onboard_profiles/superlight_profile_1.bin"
));
const SECTOR_SIZE: usize = 255;

/// A PRO X SUPERLIGHT as measured: format-4 profiles in 255-byte sectors,
/// reads past a sector refused, and a `memoryWrite` with no write started
/// refused with `InvalidArgument`, as the hardware answered when functions
/// 6 and 7 were swapped.
struct Superlight {
    unit: [u8; 4],
    directory: Vec<u8>,
    profile: Vec<u8>,
    /// The sector, size and bytes of a write `memoryAddrWrite` started.
    pending: Option<(u16, usize, Vec<u8>)>,
    /// Every `0x8100` function called, in order, with its parameters.
    onboard_calls: Vec<(u8, [u8; 16])>,
    /// Profiles `setCurrentProfile` selected.
    reselected: Vec<u8>,
}

impl Superlight {
    fn new(unit: [u8; 4]) -> Self {
        let mut directory = vec![0xff; SECTOR_SIZE];
        directory[..16].copy_from_slice(&[
            0x00, 0x01, 0x01, 0xff, 0x00, 0x02, 0x00, 0xff, 0x00, 0x03, 0x00, 0xff, 0x00, 0x04,
            0x00, 0xff,
        ]);
        Self {
            unit,
            directory,
            profile: PROFILE.to_vec(),
            pending: None,
            onboard_calls: Vec::new(),
            reselected: Vec::new(),
        }
    }

    /// The `0x8100` functions called, without their parameters.
    fn onboard_functions(&self) -> Vec<u8> {
        self.onboard_calls
            .iter()
            .map(|(function, _)| *function)
            .collect()
    }

    fn sector(&mut self, sector: u16) -> Option<&mut Vec<u8>> {
        match sector {
            0 => Some(&mut self.directory),
            1 => Some(&mut self.profile),
            _ => None,
        }
    }

    fn respond(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
            return None;
        }
        let function = request[3] >> 4;
        let mut params = [0u8; 16];
        let available = (request.len() - 4).min(16);
        params[..available].copy_from_slice(&request[4..4 + available]);
        let mut payload = [0u8; 16];
        match (request[2], function) {
            (0x00, 0x01) => payload[0] = 4,
            (0x00, 0x00) => {
                payload[0] = match u16::from_be_bytes([params[0], params[1]]) {
                    0x0003 => DEVICE_INFO_INDEX,
                    0x2201 => DPI_INDEX,
                    0x8100 => ONBOARD_INDEX,
                    _ => 0,
                };
            }
            (DEVICE_INFO_INDEX, 0x00) => payload[1..5].copy_from_slice(&self.unit),
            // One sensor; 100–25600 in steps of 50; live 1200, default 800.
            (DPI_INDEX, 0x00) => payload[0] = 1,
            (DPI_INDEX, 0x01) => {
                payload[..7].copy_from_slice(&[params[0], 0x00, 0x64, 0xe0, 0x32, 0x64, 0x00]);
            }
            (DPI_INDEX, 0x02) => payload[..5].copy_from_slice(&[params[0], 0x04, 0xb0, 0x03, 0x20]),
            (ONBOARD_INDEX, function) => {
                self.onboard_calls.push((function, params));
                if let Err(error) = self.onboard(function, &params, &mut payload) {
                    return Some(feature_error(request, error));
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

    fn onboard(
        &mut self,
        function: u8,
        params: &[u8; 16],
        payload: &mut [u8; 16],
    ) -> Result<(), u8> {
        let word = |at: usize| u16::from_be_bytes([params[at], params[at + 1]]);
        match function {
            0 => payload[..9].copy_from_slice(&[1, 4, 1, 5, 1, 5, 0x10, 0x00, 0xff]),
            2 => payload[0] = 1,
            3 => self.reselected.push(params[1]),
            4 => payload[1] = 1,
            5 => {
                let offset = usize::from(word(2));
                let sector = self.sector(word(0)).ok_or(INVALID_ARGUMENT)?;
                let chunk = sector.get(offset..offset + 16).ok_or(INVALID_ARGUMENT)?;
                payload.copy_from_slice(chunk);
            }
            6 => {
                if word(2) != 0 || self.sector(word(0)).is_none() {
                    return Err(INVALID_ARGUMENT);
                }
                self.pending = Some((word(0), usize::from(word(4)), Vec::new()));
            }
            7 => {
                let (_, _, bytes) = self.pending.as_mut().ok_or(INVALID_ARGUMENT)?;
                bytes.extend_from_slice(params);
            }
            8 => {
                let (sector, size, mut bytes) = self.pending.take().ok_or(INVALID_ARGUMENT)?;
                bytes.truncate(size);
                *self.sector(sector).ok_or(INVALID_ARGUMENT)? = bytes;
            }
            _ => return Err(INVALID_ARGUMENT),
        }
        Ok(())
    }
}

async fn superlight_channel(mouse: &Arc<Mutex<Superlight>>) -> Arc<HidppChannel> {
    let mouse = Arc::clone(mouse);
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        mouse.lock().expect("mouse lock").respond(request)
    });
    scripted_channel(raw).await
}

fn write(dpi: u16) -> OnboardDefaultDpi {
    OnboardDefaultDpi {
        unit: UNIT,
        profile: ProfileIndex::new(1).unwrap(),
        dpi: Dpi::new(dpi),
    }
}

#[tokio::test]
async fn writes_one_whole_sector_as_start_chunks_end() {
    let mouse = Arc::new(Mutex::new(Superlight::new(UNIT)));
    let channel = superlight_channel(&mouse).await;

    let change = set_onboard_default_dpi_on_channel(&channel, SLOT, write(1200))
        .await
        .unwrap();

    assert_eq!(change.before, Some(Dpi::new(4950)));
    assert_eq!(change.after, Dpi::new(1200));
    let mouse = mouse.lock().unwrap();
    let writes: Vec<&(u8, [u8; 16])> = mouse
        .onboard_calls
        .iter()
        .filter(|(function, _)| (6..=8).contains(function))
        .collect();
    // memoryAddrWrite(sector 1, offset 0, 255 bytes), sixteen memoryWrites,
    // memoryWriteEnd.
    assert_eq!(writes.len(), 18);
    assert_eq!(writes[0].0, 6);
    assert_eq!(writes[0].1[..6], [0x00, 0x01, 0x00, 0x00, 0x00, 0xff]);
    assert!(writes[1..17].iter().all(|(function, _)| *function == 7));
    assert_eq!(writes[17].0, 8);
    // The last chunk carries the sector's final 15 bytes, padded to 16.
    assert_eq!(writes[16].1[..15], mouse.profile[240..]);
    assert_eq!(writes[16].1[15], 0xff);
    let changed: Vec<usize> = (0..SECTOR_SIZE)
        .filter(|&at| PROFILE[at] != mouse.profile[at])
        .collect();
    assert_eq!(changed, [11, 12, 253, 254], "slot 4 and the CRC only");
    assert_eq!(mouse.profile[11..13], 1200u16.to_le_bytes());
    assert_eq!(
        mouse.profile[253..],
        [0x46, 0x82],
        "the CRC the hardware stored"
    );
    assert_eq!(mouse.reselected, [1], "the active profile is reloaded");
}

#[tokio::test]
async fn a_different_mouse_in_the_slot_gets_no_onboard_request() {
    let mouse = Arc::new(Mutex::new(Superlight::new([0xde, 0xad, 0xbe, 0xef])));
    let channel = superlight_channel(&mouse).await;

    assert_matches!(
        set_onboard_default_dpi_on_channel(&channel, SLOT, write(1200)).await,
        Err(WriteError::DeviceNotFound)
    );
    let mouse = mouse.lock().unwrap();
    assert!(mouse.onboard_calls.is_empty(), "nothing reaches 0x8100");
    assert_eq!(mouse.profile, PROFILE);
}

#[tokio::test]
async fn an_unsupported_dpi_gets_no_onboard_request() {
    let mouse = Arc::new(Mutex::new(Superlight::new(UNIT)));
    let channel = superlight_channel(&mouse).await;

    assert_matches!(
        set_onboard_default_dpi_on_channel(&channel, SLOT, write(1225)).await,
        Err(WriteError::DpiUnsupported { .. })
    );
    assert!(mouse.lock().unwrap().onboard_calls.is_empty());
}

#[tokio::test]
async fn a_profile_already_at_the_dpi_is_not_written() {
    let mouse = Arc::new(Mutex::new(Superlight::new(UNIT)));
    let channel = superlight_channel(&mouse).await;

    let change = set_onboard_default_dpi_on_channel(&channel, SLOT, write(4950))
        .await
        .unwrap();

    assert_eq!(change.before, Some(Dpi::new(4950)));
    let mouse = mouse.lock().unwrap();
    assert!(
        mouse
            .onboard_functions()
            .iter()
            .all(|function| !(6..=8).contains(function)),
        "only reads: {:?}",
        mouse.onboard_functions()
    );
    assert!(mouse.reselected.is_empty());
}

#[tokio::test]
async fn a_chunk_with_no_write_started_is_refused() {
    // Pins the scripted mouse to the measured answer, so the sequence test
    // above would fail if the start and chunk functions were swapped.
    let mut mouse = Superlight::new(UNIT);
    let mut payload = [0; 16];
    assert_eq!(
        mouse.onboard(7, &[0; 16], &mut payload),
        Err(INVALID_ARGUMENT)
    );
    assert_eq!(
        mouse.onboard(8, &[0; 16], &mut payload),
        Err(INVALID_ARGUMENT)
    );
}
