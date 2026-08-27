//! Peer-address hints from mDNS and manually configured host strings.
//!
//! Discovery never establishes identity. Every address returned here is only a
//! dial hint; the transport authenticates the responding machine against its
//! Ed25519 public key during mutual TLS.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    future::Future,
    io,
    net::{IpAddr, SocketAddr, SocketAddrV4, SocketAddrV6},
    pin::Pin,
    sync::{Arc, RwLock},
};

use mdns_sd::{ScopedIp, ServiceDaemon, ServiceEvent, ServiceInfo, TxtProperties};
use thiserror::Error;
use tokio::task::{JoinHandle, JoinSet};

use crate::sas::PublicKey;

/// DNS-SD service type advertised by Flow peers.
pub const SERVICE_TYPE: &str = "_openlogi-flow._udp.local.";
/// UDP port used when a manual address omits one.
///
/// mDNS candidates always use their SRV record's port instead. `42424` is an
/// unprivileged, memorable default and is not part of the frozen wire format.
pub const DEFAULT_PORT: u16 = 42_424;

const TXT_PUBLIC_KEY: &str = "pk";
const TXT_PROTO_MIN: &str = "proto_min";
const TXT_PROTO_MAX: &str = "proto_max";

/// Future returned by a dynamically dispatched [`CandidateSource`].
pub type CandidateFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, DiscoveryError>> + Send + 'a>>;

/// A source of address hints for one authenticated peer identity.
pub trait CandidateSource: Send + Sync {
    /// Returns the source's current address hints for `peer_key`.
    fn candidates(&self, peer_key: PublicKey) -> CandidateFuture<'_>;
}

/// Merges all candidate sources concurrently and removes duplicate addresses.
///
/// A failed source does not hide addresses returned by another source. An
/// error is returned only when every source failed to produce an address.
pub async fn collect_candidates(
    sources: &[Arc<dyn CandidateSource>],
    peer_key: PublicKey,
) -> Result<Vec<SocketAddr>, DiscoveryError> {
    let mut tasks = JoinSet::new();
    for source in sources {
        let source = Arc::clone(source);
        tasks.spawn(async move { source.candidates(peer_key).await });
    }

    let mut addresses = BTreeSet::new();
    let mut first_error = None;
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Ok(candidates)) => addresses.extend(candidates),
            Ok(Err(error)) => {
                first_error.get_or_insert(error);
            }
            Err(error) => {
                first_error.get_or_insert_with(|| DiscoveryError::SourceTask(error.to_string()));
            }
        }
    }

    if addresses.is_empty()
        && let Some(error) = first_error
    {
        Err(error)
    } else {
        Ok(addresses.into_iter().collect())
    }
}

/// A source that resolves manually configured IP, hostname, or `host:port` strings.
///
/// Bare values use [`DEFAULT_PORT`]. Resolution uses Tokio's OS-resolver
/// adapter, so DNS, `/etc/hosts`, and overlay-network magic DNS all apply.
#[derive(Clone, Debug)]
pub struct ManualCandidateSource {
    addresses: Arc<[String]>,
    default_port: u16,
}

impl ManualCandidateSource {
    /// Creates a source using Flow's documented default port.
    #[must_use]
    pub fn new(addresses: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::with_port(addresses, DEFAULT_PORT)
    }

    /// Creates a source with an explicit default for values that omit a port.
    #[must_use]
    pub fn with_port(
        addresses: impl IntoIterator<Item = impl Into<String>>,
        default_port: u16,
    ) -> Self {
        Self {
            addresses: addresses
                .into_iter()
                .map(Into::into)
                .collect::<Vec<_>>()
                .into(),
            default_port,
        }
    }
}

impl CandidateSource for ManualCandidateSource {
    fn candidates(&self, _peer_key: PublicKey) -> CandidateFuture<'_> {
        Box::pin(async move {
            let mut tasks = JoinSet::new();
            for address in self.addresses.iter() {
                let address = address.clone();
                let default_port = self.default_port;
                tasks.spawn(async move { resolve_manual_address(&address, default_port).await });
            }

            let mut resolved = BTreeSet::new();
            let mut first_error = None;
            while let Some(result) = tasks.join_next().await {
                match result {
                    Ok(Ok(addresses)) => resolved.extend(addresses),
                    Ok(Err(error)) => {
                        first_error.get_or_insert(error);
                    }
                    Err(error) => {
                        first_error
                            .get_or_insert_with(|| DiscoveryError::SourceTask(error.to_string()));
                    }
                }
            }

            if resolved.is_empty()
                && let Some(error) = first_error
            {
                Err(error)
            } else {
                Ok(resolved.into_iter().collect())
            }
        })
    }
}

/// Canonical Flow identity and compatibility data carried in an mDNS TXT record.
///
/// `pk` is the complete raw Ed25519 public key as exactly 64 lowercase hex
/// digits. Publishing the key rather than a truncated hash lets browsers index
/// candidates directly by their configured pin; it remains only a hint until
/// mutual TLS proves possession of the private key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MdnsRecord {
    /// Advertised machine identity.
    pub public_key: PublicKey,
    /// Lowest protocol version the peer can receive.
    pub proto_min: u32,
    /// Highest protocol version the peer can receive.
    pub proto_max: u32,
}

impl MdnsRecord {
    /// Creates and validates a record.
    pub fn new(
        public_key: PublicKey,
        proto_min: u32,
        proto_max: u32,
    ) -> Result<Self, DiscoveryError> {
        if proto_min == 0 || proto_min > proto_max {
            return Err(DiscoveryError::InvalidProtocolRange {
                proto_min,
                proto_max,
            });
        }
        Ok(Self {
            public_key,
            proto_min,
            proto_max,
        })
    }

    /// Encodes this record into canonical DNS-SD TXT key/value pairs.
    #[must_use]
    pub fn properties(self) -> HashMap<String, String> {
        HashMap::from([
            (TXT_PUBLIC_KEY.to_owned(), public_key_hex(self.public_key)),
            (TXT_PROTO_MIN.to_owned(), self.proto_min.to_string()),
            (TXT_PROTO_MAX.to_owned(), self.proto_max.to_string()),
        ])
    }

    /// Parses canonical TXT key/value pairs.
    pub fn from_properties(properties: &HashMap<String, String>) -> Result<Self, DiscoveryError> {
        let property = |key: &'static str| {
            properties
                .iter()
                .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
                .map(|(_, value)| value.as_str())
                .ok_or(DiscoveryError::MissingTxtProperty(key))
        };
        Self::from_values(
            property(TXT_PUBLIC_KEY)?,
            property(TXT_PROTO_MIN)?,
            property(TXT_PROTO_MAX)?,
        )
    }

    fn from_txt(properties: &TxtProperties) -> Result<Self, DiscoveryError> {
        let property = |key: &'static str| {
            properties
                .get_property_val_str(key)
                .ok_or(DiscoveryError::MissingTxtProperty(key))
        };
        Self::from_values(
            property(TXT_PUBLIC_KEY)?,
            property(TXT_PROTO_MIN)?,
            property(TXT_PROTO_MAX)?,
        )
    }

    fn from_values(
        public_key: &str,
        proto_min: &str,
        proto_max: &str,
    ) -> Result<Self, DiscoveryError> {
        let public_key = parse_public_key_hex(public_key)?;
        let proto_min = proto_min
            .parse()
            .map_err(|_| DiscoveryError::InvalidTxtProperty(TXT_PROTO_MIN))?;
        let proto_max = proto_max
            .parse()
            .map_err(|_| DiscoveryError::InvalidTxtProperty(TXT_PROTO_MAX))?;
        Self::new(public_key, proto_min, proto_max)
    }

    fn overlaps(self, local_min: u32, local_max: u32) -> bool {
        self.proto_max.min(local_max) >= self.proto_min.max(local_min)
    }
}

/// An active advertisement of this machine's Flow endpoint.
pub struct MdnsAdvertiser {
    daemon: ServiceDaemon,
    fullname: Option<String>,
}

impl fmt::Debug for MdnsAdvertiser {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MdnsAdvertiser")
            .field("fullname", &self.fullname)
            .finish_non_exhaustive()
    }
}

impl MdnsAdvertiser {
    /// Registers an automatically address-tracked Flow service.
    pub fn start(record: MdnsRecord, port: u16) -> Result<Self, DiscoveryError> {
        let daemon = ServiceDaemon::new()?;
        let key = public_key_hex(record.public_key);
        let short_key = &key[..16];
        let instance_name = format!("openlogi-flow-{short_key}");
        let hostname = format!("openlogi-flow-{short_key}.local.");
        let service = ServiceInfo::new(
            SERVICE_TYPE,
            &instance_name,
            &hostname,
            (),
            port,
            record.properties(),
        )?
        .enable_addr_auto();
        let fullname = service.get_fullname().to_owned();
        daemon.register(service)?;
        Ok(Self {
            daemon,
            fullname: Some(fullname),
        })
    }

    /// Returns the registered DNS-SD service fullname.
    #[must_use]
    pub fn fullname(&self) -> Option<&str> {
        self.fullname.as_deref()
    }
}

impl Drop for MdnsAdvertiser {
    fn drop(&mut self) {
        if let Some(fullname) = self.fullname.take() {
            let _ = self.daemon.unregister(&fullname);
        }
        let _ = self.daemon.shutdown();
    }
}

#[derive(Clone, Debug)]
struct DiscoveredService {
    record: MdnsRecord,
    addresses: BTreeSet<SocketAddr>,
}

/// A continuously browsing mDNS candidate source.
///
/// Resolved records outside the local protocol range are discarded before
/// they can trigger a connection attempt. The source returns snapshots of its
/// cache; the session manager retries it as discovery records change.
pub struct MdnsCandidateSource {
    daemon: ServiceDaemon,
    services: Arc<RwLock<BTreeMap<String, DiscoveredService>>>,
    browser: JoinHandle<()>,
}

impl fmt::Debug for MdnsCandidateSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MdnsCandidateSource")
            .field("services", &self.services)
            .finish_non_exhaustive()
    }
}

impl MdnsCandidateSource {
    /// Starts browsing for compatible Flow services.
    pub fn browse(local_min: u32, local_max: u32) -> Result<Self, DiscoveryError> {
        if local_min == 0 || local_min > local_max {
            return Err(DiscoveryError::InvalidProtocolRange {
                proto_min: local_min,
                proto_max: local_max,
            });
        }
        let daemon = ServiceDaemon::new()?;
        let receiver = daemon.browse(SERVICE_TYPE)?;
        let services = Arc::new(RwLock::new(BTreeMap::new()));
        let browser_services = Arc::clone(&services);
        let browser = tokio::spawn(async move {
            while let Ok(event) = receiver.recv_async().await {
                match event {
                    ServiceEvent::ServiceResolved(service) => {
                        update_resolved_service(&browser_services, &service, local_min, local_max);
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        if let Ok(mut services) = browser_services.write() {
                            services.remove(&fullname);
                        }
                    }
                    _ => {}
                }
            }
        });
        Ok(Self {
            daemon,
            services,
            browser,
        })
    }
}

impl CandidateSource for MdnsCandidateSource {
    fn candidates(&self, peer_key: PublicKey) -> CandidateFuture<'_> {
        Box::pin(async move {
            let services = self
                .services
                .read()
                .map_err(|_| DiscoveryError::CachePoisoned)?;
            Ok(services
                .values()
                .filter(|service| service.record.public_key == peer_key)
                .flat_map(|service| service.addresses.iter().copied())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect())
        })
    }
}

impl Drop for MdnsCandidateSource {
    fn drop(&mut self) {
        self.browser.abort();
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        let _ = self.daemon.shutdown();
    }
}

/// Errors produced by candidate discovery and manual resolution.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    /// The mDNS daemon rejected an operation or record.
    #[error("mDNS discovery failed: {0}")]
    Mdns(#[from] mdns_sd::Error),
    /// A manual hostname could not be resolved.
    #[error("failed to resolve manual address {address}: {source}")]
    Resolve {
        /// Address string supplied by configuration.
        address: String,
        /// OS resolver failure.
        source: io::Error,
    },
    /// A manual address has an invalid or ambiguous shape.
    #[error("invalid manual address {0}")]
    InvalidManualAddress(String),
    /// An advertised protocol range is malformed.
    #[error("invalid protocol range {proto_min}..={proto_max}")]
    InvalidProtocolRange {
        /// Advertised lower bound.
        proto_min: u32,
        /// Advertised upper bound.
        proto_max: u32,
    },
    /// A required TXT property is absent.
    #[error("mDNS TXT record is missing {0}")]
    MissingTxtProperty(&'static str),
    /// A TXT property is not in its canonical representation.
    #[error("mDNS TXT property {0} is invalid")]
    InvalidTxtProperty(&'static str),
    /// A candidate-source task failed before returning its result.
    #[error("candidate source task failed: {0}")]
    SourceTask(String),
    /// The mDNS cache's synchronization primitive was poisoned.
    #[error("mDNS candidate cache is unavailable")]
    CachePoisoned,
}

async fn resolve_manual_address(
    address: &str,
    default_port: u16,
) -> Result<Vec<SocketAddr>, DiscoveryError> {
    let address = address.trim();
    if address.is_empty() {
        return Err(DiscoveryError::InvalidManualAddress(address.to_owned()));
    }
    if let Ok(socket) = address.parse::<SocketAddr>() {
        return Ok(vec![socket]);
    }
    if let Ok(ip) = address.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, default_port)]);
    }
    if let Some(bracketed) = address
        .strip_prefix('[')
        .and_then(|address| address.strip_suffix(']'))
    {
        let ip = bracketed
            .parse::<IpAddr>()
            .map_err(|_| DiscoveryError::InvalidManualAddress(address.to_owned()))?;
        return Ok(vec![SocketAddr::new(ip, default_port)]);
    }

    let colon_count = address.bytes().filter(|byte| *byte == b':').count();
    let (hostname, port) = match colon_count {
        0 => (address, default_port),
        1 => {
            let (hostname, port) = address
                .rsplit_once(':')
                .ok_or_else(|| DiscoveryError::InvalidManualAddress(address.to_owned()))?;
            let port = port
                .parse::<u16>()
                .map_err(|_| DiscoveryError::InvalidManualAddress(address.to_owned()))?;
            if hostname.is_empty() {
                return Err(DiscoveryError::InvalidManualAddress(address.to_owned()));
            }
            (hostname, port)
        }
        _ => return Err(DiscoveryError::InvalidManualAddress(address.to_owned())),
    };
    let addresses = tokio::net::lookup_host((hostname, port))
        .await
        .map_err(|source| DiscoveryError::Resolve {
            address: address.to_owned(),
            source,
        })?;
    Ok(addresses.collect())
}

fn update_resolved_service(
    services: &RwLock<BTreeMap<String, DiscoveredService>>,
    service: &mdns_sd::ResolvedService,
    local_min: u32,
    local_max: u32,
) {
    let Ok(mut services) = services.write() else {
        return;
    };
    services.remove(service.get_fullname());
    let Ok(record) = MdnsRecord::from_txt(service.get_properties()) else {
        return;
    };
    if service.get_port() == 0 || !record.overlaps(local_min, local_max) {
        return;
    }
    let port = service.get_port();
    let addresses = service
        .get_addresses()
        .iter()
        .filter_map(|address| match address {
            ScopedIp::V4(address) => Some(SocketAddr::V4(SocketAddrV4::new(*address.addr(), port))),
            ScopedIp::V6(address) => Some(SocketAddr::V6(SocketAddrV6::new(
                *address.addr(),
                port,
                0,
                address.scope_id().index,
            ))),
            _ => None,
        })
        .collect();
    services.insert(
        service.get_fullname().to_owned(),
        DiscoveredService { record, addresses },
    );
}

fn public_key_hex(public_key: PublicKey) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in public_key.as_bytes() {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn parse_public_key_hex(encoded: &str) -> Result<PublicKey, DiscoveryError> {
    if encoded.len() != 64
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(DiscoveryError::InvalidTxtProperty(TXT_PUBLIC_KEY));
    }
    let mut key = [0_u8; 32];
    for (index, pair) in encoded.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        key[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Ok(PublicKey::new(key))
}

fn hex_nibble(byte: u8) -> Result<u8, DiscoveryError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(DiscoveryError::InvalidTxtProperty(TXT_PUBLIC_KEY)),
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use tokio::sync::Barrier;

    use super::*;

    struct FakeSource {
        barrier: Arc<Barrier>,
        result: Result<Vec<SocketAddr>, &'static str>,
    }

    impl CandidateSource for FakeSource {
        fn candidates(&self, _peer_key: PublicKey) -> CandidateFuture<'_> {
            Box::pin(async move {
                self.barrier.wait().await;
                self.result
                    .clone()
                    .map_err(|detail| DiscoveryError::SourceTask(detail.to_owned()))
            })
        }
    }

    #[test]
    fn mdns_record_round_trips_canonical_txt_properties() {
        let record = MdnsRecord::new(PublicKey::new([0xab; 32]), 1, 3).unwrap();
        let properties = record.properties();
        assert_eq!(properties[TXT_PUBLIC_KEY], "ab".repeat(32));
        assert_eq!(MdnsRecord::from_properties(&properties).unwrap(), record);

        let mut uppercase = properties;
        uppercase.insert(TXT_PUBLIC_KEY.to_owned(), "AB".repeat(32));
        assert!(matches!(
            MdnsRecord::from_properties(&uppercase),
            Err(DiscoveryError::InvalidTxtProperty(TXT_PUBLIC_KEY))
        ));
    }

    #[test]
    fn protocol_overlap_filters_disjoint_records() {
        let record = MdnsRecord::new(PublicKey::new([1; 32]), 2, 4).unwrap();
        assert!(record.overlaps(1, 2));
        assert!(!record.overlaps(5, 6));
    }

    #[tokio::test]
    async fn manual_addresses_use_default_and_explicit_ports() {
        let source =
            ManualCandidateSource::with_port(["127.0.0.1", "127.0.0.1:5000", "[::1]"], 4000);
        let addresses = source.candidates(PublicKey::new([0; 32])).await.unwrap();
        assert_eq!(
            addresses.into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "127.0.0.1:4000".parse().unwrap(),
                "127.0.0.1:5000".parse().unwrap(),
                "[::1]:4000".parse().unwrap(),
            ])
        );
    }

    #[tokio::test]
    async fn candidate_sources_are_polled_concurrently_and_deduplicated() {
        let barrier = Arc::new(Barrier::new(3));
        let repeated = "127.0.0.1:4000".parse().unwrap();
        let unique = "127.0.0.1:5000".parse().unwrap();
        let sources: Vec<Arc<dyn CandidateSource>> = vec![
            Arc::new(FakeSource {
                barrier: Arc::clone(&barrier),
                result: Ok(vec![repeated]),
            }),
            Arc::new(FakeSource {
                barrier: Arc::clone(&barrier),
                result: Ok(vec![repeated, unique]),
            }),
        ];

        let collected =
            tokio::spawn(
                async move { collect_candidates(&sources, PublicKey::new([1; 32])).await },
            );
        tokio::time::timeout(Duration::from_secs(1), barrier.wait())
            .await
            .unwrap();
        assert_eq!(collected.await.unwrap().unwrap(), [repeated, unique]);
    }

    #[tokio::test]
    #[ignore = "requires multicast-capable host networking"]
    async fn live_mdns_round_trip() {
        let key = PublicKey::new([0x42; 32]);
        let source = MdnsCandidateSource::browse(1, 1).unwrap();
        let _advertiser =
            MdnsAdvertiser::start(MdnsRecord::new(key, 1, 1).unwrap(), 42_424).unwrap();

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !source.candidates(key).await.unwrap().is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
    }
}
