//! The Logitech G700: reaching it, dumping its settings registers and
//! onboard memory, and writing them (see `configure`), plus the live DPI and
//! report-rate settings the generic DPI and report-rate entry points route
//! here (see `live`).
//!
//! The G700 is a HID++ 1.0 mouse: no feature table, settings at fixed
//! registers, five profiles in onboard flash. It is found like every other
//! device — by the inventory, whose paired device's route
//! [`find_g700_route`] picks out — and addressed through [`G700Link`], which
//! holds the node's register phase for as long as it lives. Which HID++
//! address is a G700 is `openlogi_device_registry::g700`'s decision.
//! Decoding is left to the codecs in [`hidpp::register`], which callers
//! reach as [`crate::g700`]; every request goes through this module.

mod configure;
mod live;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use hidpp::{
    channel::HidppChannel,
    protocol::v10::Hidpp10Error,
    register::{
        battery::{self, StatusReading},
        current_profile::{CurrentProfile, read_current_profile},
        onboard_memory::{
            LAST_PAGE, Page, directory::DIRECTORY_PAGE, g700, read_memory, read_page,
        },
        report_rate::{ReportInterval, read_report_interval},
        resolution::{Resolution, read_current_resolution},
    },
};
use openlogi_core::device::DeviceInventory;
use openlogi_device_registry::g700::{G700Link as Link, g700_link, is_g700};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::{HidBackend, NodeId};
use crate::channel::route::{DeviceRoute, open_route_shared};
use crate::host_lock::ReceiverRegisterPhase;

use super::WriteError;

pub use configure::{
    BackupOrigin, G700Error, G700Register, ProfileDpiChange, ProfileDpiWrite, ProfileRateChange,
    ProfileRateWrite, RestoreOutcome, RestoredPage, restore_g700_pages, set_g700_current_profile,
    set_g700_profile_dpi, set_g700_profile_report_interval, set_g700_report_interval,
    set_g700_resolution,
};
pub(crate) use live::{
    dpi_info as g700_dpi_info, report_rate_info as g700_report_rate_info,
    set_dpi_target as set_g700_dpi_target, set_report_rate as set_g700_report_rate,
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

impl G700MemoryDump {
    /// The identity of the mouse this dump was taken from, for a backup to
    /// carry (see [`G700Identity`]).
    #[must_use]
    pub fn identity(&self) -> G700Identity {
        G700Identity::of_pages(
            self.route.to_string(),
            self.device_index,
            self.pages
                .iter()
                .filter_map(|read| Some((read.page, read.result.as_ref().ok()?))),
        )
    }
}

/// What tells one G700 from another well enough for a restore to refuse a
/// backup of another mouse: where it was reached, how much onboard memory
/// reads, and where its directory places its profiles. No register names the
/// mouse itself — its receiver refuses the pairing-information register,
/// and on the cable there is none — so two mice paired to one receiver slot
/// with the same memory layout are not told apart.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct G700Identity {
    /// The route, as [`DeviceRoute`]'s `Display` writes it: receiver uid and
    /// slot, or the cable's USB id.
    pub route: String,
    /// The HID++ device index the mouse answered at.
    pub device_index: u8,
    /// How many of the pages `0..=`[`LAST_PAGE`] read.
    pub readable_pages: usize,
    /// The `(page, offset)` of each profile the directory lists, in order.
    pub directory: Vec<(u8, u8)>,
}

impl G700Identity {
    /// The identity of the mouse at `route` and `device_index` whose
    /// readable pages are `pages`, as a dump or a backup of it holds them.
    #[must_use]
    pub fn of_pages<'a>(
        route: String,
        device_index: u8,
        pages: impl IntoIterator<Item = (u8, &'a Page)>,
    ) -> Self {
        let mut readable_pages = 0;
        let mut directory = Vec::new();
        for (number, page) in pages {
            readable_pages += 1;
            if number == DIRECTORY_PAGE {
                directory = directory_places(page);
            }
        }
        Self {
            route,
            device_index,
            readable_pages,
            directory,
        }
    }
}

fn directory_places(page: &Page) -> Vec<(u8, u8)> {
    g700::parse_directory(page)
        .entries()
        .iter()
        .map(|entry| (entry.page, entry.offset))
        .collect()
}

/// The route of the first G700 among `inventories`, as a probe of this host
/// found them, or `None` when there is none.
#[must_use]
pub fn find_g700_route(inventories: &[DeviceInventory]) -> Option<DeviceRoute> {
    inventories.iter().find_map(|inventory| {
        let receiver = &inventory.receiver;
        inventory
            .paired
            .iter()
            .find(|paired| is_g700(receiver.vendor_id, receiver.product_id, paired.slot))
            .and_then(|paired| DeviceRoute::for_slot(inventory, paired.slot))
    })
}

/// A G700 reached on an open channel, ready for register traffic.
pub(crate) struct G700Link {
    chan: Arc<HidppChannel>,
    route: DeviceRoute,
    device_index: u8,
    /// The node's register phase, held for as long as the link lives:
    /// HID++ 1.0 replies carry no software id, and every `0xA2` reply has the
    /// same header, so another process's register traffic on the node must
    /// wait or defer meanwhile.
    _registers: ReceiverRegisterPhase,
}

impl G700Link {
    /// The G700 `route` addresses. [`WriteError::DeviceNotFound`] when
    /// nothing answers to the route or what does is not a G700.
    pub(crate) async fn open(
        backend: &dyn HidBackend,
        route: &DeviceRoute,
    ) -> Result<Self, WriteError> {
        let Some(shared) = open_route_shared(backend, route).await? else {
            return Err(WriteError::DeviceNotFound);
        };
        Self::on_shared(&shared)
            .await?
            .ok_or(WriteError::DeviceNotFound)
    }

    /// The G700 `shared` reaches, or `None` when it reaches another device —
    /// decided from the node's USB id and the route's index alone, without
    /// a request. Takes the node's register phase; fails when another
    /// OpenLogi process holds it past its wait, or, on the cable, when the
    /// mouse answers at none of its indexes.
    pub(crate) async fn on_shared(shared: &SharedChannel) -> Result<Option<Self>, WriteError> {
        let chan = shared.channel();
        let Some(link) = g700_link(chan.vendor_id, chan.product_id)
            .filter(|link| link.addresses_g700(shared.device_index()))
        else {
            return Ok(None);
        };
        let Some(registers) = shared.lock_registers().await else {
            return Err(WriteError::Hidpp(
                "another OpenLogi process or task holds the G700's register phase".into(),
            ));
        };
        let device_index = match link.device_indexes() {
            [index] => *index,
            candidates => answering_index(chan, shared.node(), candidates).await?,
        };
        Ok(Some(Self {
            chan: Arc::clone(chan),
            route: shared.route().clone(),
            device_index,
            _registers: registers,
        }))
    }

    pub(crate) fn chan(&self) -> &HidppChannel {
        &self.chan
    }

    pub(crate) fn device_index(&self) -> u8 {
        self.device_index
    }
}

/// The index a G700 on its cable answers at (see
/// [`openlogi_device_registry::g700::G700Link::Cable`]).
/// The inventory probe of the cable on `node` asks this too. The caller
/// holds the node's register phase.
pub(crate) async fn wired_device_index(
    chan: &HidppChannel,
    node: &NodeId,
) -> Result<u8, WriteError> {
    answering_index(chan, node, Link::Cable.device_indexes()).await
}

/// The index the G700 on each HID node last answered at, tried first next
/// time: a candidate that stays silent costs a whole request timeout, which
/// every probe pass and every agent operation on the cable would otherwise
/// pay again. The answer belongs to the OS node, so a mouse that comes back
/// as a different node is asked afresh; an index that stops answering is
/// passed over and the next one that answers replaces it.
static ANSWERED_AT: LazyLock<Mutex<HashMap<NodeId, u8>>> = LazyLock::new(Mutex::default);

/// The first of `candidates` whose current-profile register answers, with
/// its value or with a register error — either way a device is there. The
/// index `node` last answered at goes first.
async fn answering_index(
    chan: &HidppChannel,
    node: &NodeId,
    candidates: &[u8],
) -> Result<u8, WriteError> {
    let answered_at = || ANSWERED_AT.lock().unwrap_or_else(PoisonError::into_inner);
    let last = answered_at()
        .get(node)
        .copied()
        .filter(|index| candidates.contains(index));
    let ordered = last.into_iter().chain(
        candidates
            .iter()
            .copied()
            .filter(|index| Some(*index) != last),
    );
    for index in ordered {
        match read_current_profile(chan, index).await {
            Ok(_) | Err(Hidpp10Error::RegisterAccess(_)) => {
                answered_at().insert(node.clone(), index);
                return Ok(index);
            }
            Err(error) => debug!(index, ?error, "no G700 answers at this index"),
        }
    }
    answered_at().remove(node);
    Err(WriteError::DeviceUnreachable {
        index: candidates.first().copied().unwrap_or_default(),
    })
}

/// Dump the G700 at `route`; read-only.
///
/// The dump holds the node's register phase to the last page, some
/// seconds, so another OpenLogi process's register traffic on that node
/// waits or defers meanwhile.
///
/// [`WriteError::DeviceUnreachable`] when none of the settings registers
/// answers — the mouse is off, asleep, or not paired.
pub async fn read_g700_memory(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<G700MemoryDump, WriteError> {
    let link = G700Link::open(backend, route).await?;
    read_g700_memory_on_channel(link.chan(), link.route.clone(), link.device_index).await
}

/// The identity of the G700 `link` reaches, as [`G700MemoryDump::identity`]
/// takes it from a dump: the first chunk of every page tells which read.
async fn read_identity(link: &G700Link) -> Result<G700Identity, configure::G700Error> {
    let mut readable_pages = 0;
    for page in 0..=LAST_PAGE {
        if read_memory(link.chan(), link.device_index, page, 0)
            .await
            .is_ok()
        {
            readable_pages += 1;
        }
    }
    let directory = read_page(link.chan(), link.device_index, DIRECTORY_PAGE)
        .await
        .map_err(|source| configure::G700Error::PageRead {
            page: DIRECTORY_PAGE,
            source,
        })?;
    Ok(G700Identity {
        route: link.route.to_string(),
        device_index: link.device_index,
        readable_pages,
        directory: directory_places(&directory),
    })
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

    use openlogi_core::device::{DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo};

    use super::{
        G700Identity, G700Link, find_g700_route, read_g700_memory, read_g700_memory_on_channel,
    };
    use crate::SharedChannel;
    use crate::backend::NodeId;
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
        // A node with no HID++ collection: nothing answers to the route.
        let backend =
            ScriptedBackend::new(vec![(scripted_node_info("mx"), ScriptedOpen::NotHidpp)]);

        let result = read_g700_memory(&*backend, &receiver_route()).await;

        assert!(
            matches!(result, Err(WriteError::DeviceNotFound)),
            "{result:?}"
        );
    }

    fn inventory(product_id: u16, slots: &[u8]) -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "receiver".into(),
                vendor_id: 0x046d,
                product_id,
                unique_id: Some("500F9BB6".into()),
            },
            paired: slots
                .iter()
                .map(|&slot| PairedDevice {
                    slot,
                    codename: None,
                    wpid: None,
                    kind: DeviceKind::Mouse,
                    online: true,
                    battery: None,
                    model_info: None,
                    capabilities: None,
                    settings_revision: 0,
                })
                .collect(),
        }
    }

    #[test]
    fn the_g700_route_is_its_receiver_slot_or_its_cable() {
        let bolt = inventory(0xc548, &[1]);
        let nano = inventory(0xc531, &[1]);
        assert_eq!(
            find_g700_route(&[bolt.clone(), nano]),
            Some(DeviceRoute::Unifying {
                receiver_uid: "500F9BB6".into(),
                slot: 1
            }),
            "slot 1 of a Bolt receiver is another mouse"
        );
        let mut cable = inventory(0xc06b, &[0xff]);
        cable.receiver.unique_id = None;
        assert_eq!(
            find_g700_route(&[cable]),
            Some(DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xc06b
            })
        );
        assert_eq!(find_g700_route(&[bolt, inventory(0xc531, &[2])]), None);
    }

    #[tokio::test]
    async fn another_device_is_no_g700_link_and_hears_nothing() {
        let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(|_| None);
        let shared = SharedChannel::new(
            scripted_channel(raw.presenting_as(0xc548)).await,
            DeviceRoute::Bolt {
                receiver_uid: "bolt".into(),
                slot: 1,
            },
        );

        assert!(G700Link::on_shared(&shared).await.unwrap().is_none());
        assert!(handle.written_reports().is_empty());
    }

    #[tokio::test]
    async fn the_cable_is_addressed_at_the_first_index_that_answers() {
        // Only index 0x00 answers, as libratbag addresses a wired HID++ 1.0
        // mouse; 0xFF stays silent.
        let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(|request| {
            (request[1] == 0x00).then(|| vec![0x10, 0x00, 0x81, 0x0f, 0x00, 0x02, 0x00])
        });
        let raw = raw.presenting_as(0xc06b);
        // A node of its own: the index a node answered at outlives the link.
        let shared = SharedChannel::on_node(
            scripted_channel(raw).await,
            DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xc06b,
            },
            NodeId::from(format!("g700-cable-{}", std::process::id())),
        );
        let indexes = || -> Vec<u8> {
            handle
                .written_reports()
                .iter()
                .map(|report| report[1])
                .collect()
        };

        let link = G700Link::on_shared(&shared).await.unwrap().unwrap();

        assert_eq!(link.device_index(), 0x00);
        assert_eq!(indexes(), [0xff, 0x00], "0xFF first, then 0x00");
        drop(link);

        // The next link on the node asks the index that answered first, so
        // it does not wait out 0xFF's silence again.
        let link = G700Link::on_shared(&shared).await.unwrap().unwrap();

        assert_eq!(link.device_index(), 0x00);
        assert_eq!(indexes(), [0xff, 0x00, 0x00]);
    }

    #[tokio::test]
    async fn a_dump_names_the_mouse_it_was_taken_from() {
        let mut memory = vec![[0xffu8; 512]; 32];
        memory[1][..6].copy_from_slice(&[0x02, 0x00, 0x01, 0x03, 0x00, 0x03]);
        let image = memory.clone();
        let (raw, _) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            // Pages 27 to 31 do not read, as on the G700 the dumps came from.
            if request[2..4] == [0x83, 0xa2] && request[4] >= 27 {
                return Some(register_error(request, 0x0b));
            }
            respond(&image, request)
        });
        let chan = scripted_channel(raw).await;

        let dump = read_g700_memory_on_channel(&chan, receiver_route(), 1)
            .await
            .unwrap();

        assert_eq!(
            dump.identity(),
            G700Identity {
                route: "slot 1 on receiver uid".into(),
                device_index: 1,
                readable_pages: 27,
                directory: vec![(2, 0), (3, 0)],
            }
        );
    }
}
