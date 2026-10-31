//! The Logitech G700: finding it, dumping its settings registers and onboard
//! memory, and writing them (see `configure`).
//!
//! The G700 is a HID++ 1.0 mouse: no feature table, settings at fixed
//! registers, five profiles in onboard flash. The mouse is found here rather
//! than through an inventory probe, which may set the receiver's notification
//! flags: finding it sends only `GET_REGISTER`/`GET_LONG_REGISTER` requests,
//! so the dump changes nothing. Decoding is left to the codecs in
//! [`hidpp::register`], which callers reach as [`crate::g700`]; every
//! request goes through this module.

mod configure;

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    protocol::v10::Hidpp10Error,
    receiver::{self, Receiver},
    register::{
        battery::{self, StatusReading},
        current_profile::{CurrentProfile, read_current_profile},
        onboard_memory::{LAST_PAGE, Page, directory::DIRECTORY_PAGE, g700, read_page},
        report_rate::{ReportInterval, read_report_interval},
        resolution::{Resolution, read_current_resolution},
    },
};
use openlogi_device_registry::g700::{
    G700_RECEIVER_PRODUCT_ID, G700_RECEIVER_SLOT, G700_WIRED_PRODUCT_ID,
};
use tracing::debug;

use crate::backend::{HidBackend, NodeInfo};
use crate::channel::route::{DIRECT_DEVICE_INDEX, DeviceRoute, LOGITECH_VENDOR_ID};
use crate::host_lock::{RECEIVER_REGISTER_TIMEOUT, ReceiverRegisterPhase, lock_receiver_registers};

use super::WriteError;

pub use configure::{
    G700Error, G700Register, ProfileDpiChange, ProfileDpiWrite, RestoreOutcome, RestoredPage,
    restore_g700_pages, set_g700_current_profile, set_g700_profile_dpi, set_g700_report_interval,
    set_g700_resolution,
};

/// One page read of a [`G700MemoryDump`].
#[derive(Debug)]
pub struct PageRead {
    /// The page number.
    pub page: u8,
    /// The page, or why the device did not hand it over.
    pub result: Result<Page, Hidpp10Error>,
}

/// Everything [`read_g700_memory`] read. A failed read is kept with its
/// error instead of ending the dump, so a backup holds every page that did
/// read.
#[derive(Debug)]
pub struct G700MemoryDump {
    /// Where the mouse was found.
    pub route: DeviceRoute,
    /// The HID++ device index the reads addressed.
    pub device_index: u8,
    /// Register `0x0F`.
    pub current_profile: Result<CurrentProfile, Hidpp10Error>,
    /// Register `0x63`.
    pub resolution: Result<Resolution, Hidpp10Error>,
    /// Register `0x64`.
    pub report_interval: Result<ReportInterval, Hidpp10Error>,
    /// Register `0x07`.
    pub battery: Result<StatusReading, Hidpp10Error>,
    /// Pages `0..=`[`LAST_PAGE`], in order.
    pub pages: Vec<PageRead>,
    /// A second read of the directory page and of every page its entries
    /// name, taken after `pages`. Register `0xA2`'s replies all share one
    /// header, so a late reply can stand in for another chunk; a page that
    /// reads the same twice was not shifted that way.
    pub rereads: Vec<PageRead>,
}

/// A G700 found and ready for register traffic.
struct G700Link {
    chan: Arc<HidppChannel>,
    route: DeviceRoute,
    device_index: u8,
    /// Behind a receiver, its register phase, held for as long as the link
    /// lives: HID++ 1.0 replies carry no software id, and every `0xA2` reply
    /// has the same header, so another process's register reads on that
    /// receiver must wait or defer meanwhile.
    _registers: Option<ReceiverRegisterPhase>,
}

/// Find a G700 — behind its own Nano receiver, or on its cable.
///
/// Behind the receiver the mouse is addressed at [`G700_RECEIVER_SLOT`]:
/// that receiver refuses the pairing-information register, and learning the
/// slot from arrival notifications would mean a register write to trigger
/// them. A receiver whose register phase another process holds is passed
/// over.
async fn find_g700(backend: &dyn HidBackend) -> Result<G700Link, WriteError> {
    for node in backend.enumerate_hidpp().await? {
        if node.vendor_id != LOGITECH_VENDOR_ID {
            continue;
        }
        let link = match node.product_id {
            G700_WIRED_PRODUCT_ID => {
                let Some(chan) = backend.open_hidpp(&node).await? else {
                    continue;
                };
                G700Link {
                    chan,
                    route: DeviceRoute::Direct {
                        vendor_id: node.vendor_id,
                        product_id: node.product_id,
                    },
                    device_index: DIRECT_DEVICE_INDEX,
                    _registers: None,
                }
            }
            G700_RECEIVER_PRODUCT_ID => match link_behind_receiver(backend, &node).await? {
                Some(link) => link,
                None => continue,
            },
            _ => continue,
        };
        return Ok(link);
    }
    Err(WriteError::DeviceNotFound)
}

/// Find a G700 and dump it; read-only.
///
/// Through the receiver the dump holds the receiver's register phase to the
/// last page, some seconds, so another OpenLogi process's register reads on
/// that receiver wait or defer meanwhile.
///
/// [`WriteError::DeviceUnreachable`] when a receiver is there but none of the
/// settings registers answers — the mouse is off, asleep, or not paired.
pub async fn read_g700_memory(backend: &dyn HidBackend) -> Result<G700MemoryDump, WriteError> {
    let link = find_g700(backend).await?;
    read_g700_memory_on_channel(&link.chan, link.route.clone(), link.device_index).await
}

/// The G700 behind the receiver on `node`; `None` when the node is not that
/// receiver or another process holds its register phase.
async fn link_behind_receiver(
    backend: &dyn HidBackend,
    node: &NodeInfo,
) -> Result<Option<G700Link>, WriteError> {
    let Some(chan) = backend.open_hidpp(node).await? else {
        return Ok(None);
    };
    let Some(Receiver::Unifying(unifying)) = receiver::detect(Arc::clone(&chan)) else {
        return Ok(None);
    };
    let Some(registers) = lock_receiver_registers(&node.id, RECEIVER_REGISTER_TIMEOUT).await else {
        debug!(node = %node.id, "receiver register phase held elsewhere — passing the node over");
        return Ok(None);
    };
    let receiver_uid = match unifying.get_unique_id().await {
        Ok(uid) => uid,
        Err(error) => {
            debug!(node = %node.id, ?error, "receiver unique id unreadable — passing the node over");
            return Ok(None);
        }
    };
    Ok(Some(G700Link {
        chan,
        route: DeviceRoute::Unifying {
            receiver_uid,
            slot: G700_RECEIVER_SLOT,
        },
        device_index: G700_RECEIVER_SLOT,
        _registers: Some(registers),
    }))
}

async fn read_g700_memory_on_channel(
    chan: &HidppChannel,
    route: DeviceRoute,
    device_index: u8,
) -> Result<G700MemoryDump, WriteError> {
    let current_profile = read_current_profile(chan, device_index).await;
    let resolution = read_current_resolution(chan, device_index).await;
    let report_interval = read_report_interval(chan, device_index).await;
    let battery = battery::read_status(chan, device_index).await;
    if current_profile.is_err()
        && resolution.is_err()
        && report_interval.is_err()
        && battery.is_err()
    {
        debug!(
            device_index,
            ?current_profile,
            "no settings register answered — no G700 here"
        );
        return Err(WriteError::DeviceUnreachable {
            index: device_index,
        });
    }

    let mut pages = Vec::with_capacity(usize::from(LAST_PAGE) + 1);
    for page in 0..=LAST_PAGE {
        pages.push(PageRead {
            page,
            result: read_page(chan, device_index, page).await,
        });
    }

    let mut reread_pages = vec![DIRECTORY_PAGE];
    if let Some(Ok(directory)) = pages
        .get(usize::from(DIRECTORY_PAGE))
        .map(|read| read.result.as_ref())
    {
        reread_pages.extend(
            g700::parse_directory(directory)
                .entries()
                .iter()
                .map(|entry| entry.page),
        );
    }
    let mut rereads = Vec::with_capacity(reread_pages.len());
    for page in reread_pages {
        rereads.push(PageRead {
            page,
            result: read_page(chan, device_index, page).await,
        });
    }

    Ok(G700MemoryDump {
        route,
        device_index,
        current_profile,
        resolution,
        report_interval,
        battery,
        pages,
        rereads,
    })
}

#[cfg(test)]
mod tests {
    use hidpp::register::current_profile::CurrentProfile;

    use super::{read_g700_memory, read_g700_memory_on_channel};
    use crate::channel::route::DeviceRoute;
    use crate::channel::scripted::{
        ScriptedBackend, ScriptedOpen, ScriptedRawHidChannel, register_error, scripted_channel,
        scripted_node_info,
    };
    use crate::write::WriteError;

    fn receiver_route() -> DeviceRoute {
        DeviceRoute::Unifying {
            receiver_uid: "uid".into(),
            slot: 1,
        }
    }

    /// A G700 answering the dump's reads from `memory`; anything else goes
    /// unanswered.
    fn respond(memory: &[[u8; 512]], request: &[u8]) -> Option<Vec<u8>> {
        let [0x10, device, sub_id, address, p0, p1, _] = *request else {
            return None;
        };
        let mut reply = vec![0x10, device, sub_id, address];
        match (sub_id, address) {
            (0x81, 0x0f) => reply.extend([0x00, 0x01, 0x00]),
            (0x81, 0x64) => reply.extend([0x02, 0x00, 0x00]),
            (0x81, 0x07) => reply.extend([0x07, 0x00, 0x00]),
            (0x83, 0x63) => {
                reply[0] = 0x11;
                reply.extend([0x44, 0x00, 0x44, 0x00]);
                reply.resize(20, 0);
            }
            (0x83, 0xa2) => {
                let start = usize::from(p1) * 2;
                reply[0] = 0x11;
                reply.extend_from_slice(&memory[usize::from(p0)][start..start + 16]);
            }
            _ => return None,
        }
        Some(reply)
    }

    #[tokio::test]
    async fn reads_every_page_and_rereads_the_listed_ones_without_writing() {
        let mut memory = vec![[0xffu8; 512]; 32];
        memory[1][..6].copy_from_slice(&[0x02, 0x00, 0x00, 0x05, 0x00, 0x00]);
        for (page, bytes) in memory.iter_mut().enumerate() {
            bytes[100] = u8::try_from(page).unwrap();
        }
        let image = memory.clone();
        let (raw, handle) =
            ScriptedRawHidChannel::with_dynamic_responder(move |request| respond(&image, request));
        let chan = scripted_channel(raw).await;

        let dump = read_g700_memory_on_channel(&chan, receiver_route(), 1)
            .await
            .unwrap();

        assert_eq!(dump.current_profile.unwrap(), CurrentProfile::Index(1));
        assert_eq!(dump.resolution.unwrap().x, 0x44);
        assert_eq!(dump.report_interval.unwrap().rate_hz(), 500);
        assert_eq!(dump.pages.len(), 32);
        for read in &dump.pages {
            let page = read.result.as_ref().unwrap();
            assert_eq!(
                page.as_bytes(),
                &memory[usize::from(read.page)],
                "page {}",
                read.page
            );
        }
        let reread: Vec<u8> = dump.rereads.iter().map(|read| read.page).collect();
        assert_eq!(reread, [1, 2, 5], "the directory, then each listed page");
        let written = handle.written_reports();
        assert!(
            written
                .iter()
                .all(|report| report[0] == 0x10 && matches!(report[2], 0x81 | 0x83)),
            "only short GET_REGISTER and GET_LONG_REGISTER requests go out"
        );
    }

    #[tokio::test]
    async fn a_slot_that_answers_no_settings_register_is_unreachable() {
        // The receiver answering for a sleeping or absent mouse.
        let (raw, handle) =
            ScriptedRawHidChannel::with_responder(|request| Some(register_error(request, 0x09)));
        let chan = scripted_channel(raw).await;

        let result = read_g700_memory_on_channel(&chan, receiver_route(), 1).await;

        assert!(
            matches!(result, Err(WriteError::DeviceUnreachable { index: 1 })),
            "{result:?}"
        );
        assert_eq!(handle.written_reports().len(), 4, "no page is read");
    }

    #[tokio::test]
    async fn a_host_without_a_g700_has_nothing_to_dump() {
        // Another Logitech device: not opened, since its product id rules it out.
        let backend = ScriptedBackend::new(vec![(scripted_node_info("mx"), ScriptedOpen::Fails)]);

        let result = read_g700_memory(&*backend).await;

        assert!(
            matches!(result, Err(WriteError::DeviceNotFound)),
            "{result:?}"
        );
    }
}
