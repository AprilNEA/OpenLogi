//! Built-in asset mirror discovery and npm shard routing.
//!
//! A synchronization run races the production custom domain, its versioned
//! Cloudflare Pages alias, and the fixed jsDelivr npm release. The first
//! source with a complete valid catalog supplies both `index.json` and every
//! subsequent file URL for that run, so caches never mix mirrors mid-sync.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::mpsc;
use std::thread;

use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::error::AssetError;
use crate::http::{AssetClient, write_replace};
use crate::index::Index;

const INDEX_NAME: &str = "index.json";

/// Mutable production endpoint behind the OpenLogi custom domain.
const PRODUCTION_BASE: &str = "https://assets.openlogi.org";

/// Stable Cloudflare Pages branch alias for asset release 0.0.1.
const PAGES_BASE: &str = "https://v0-0-1.openlogi-assets.pages.dev";

/// Exact jsDelivr catalog package for asset release 0.0.1.
const JSDELIVR_CATALOG_BASE: &str = "https://cdn.jsdelivr.net/npm/@logi-assets/catalog@0.0.1";

/// jsDelivr prefix shared by every npm asset shard.
const JSDELIVR_PACKAGE_ROOT: &str = "https://cdn.jsdelivr.net/npm";

/// npm asset release this OpenLogi build understands.
const ASSET_VERSION: &str = "0.0.1";

/// Filename and schema of the depot-to-package routing catalog.
const NPM_ROUTES_NAME: &str = "npm-routes.json";
const NPM_ROUTES_SCHEMA: u32 = 1;

/// Asset endpoint selected for one synchronization run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssetSource {
    /// Mutable production custom domain.
    Production,
    /// Versioned Cloudflare Pages branch alias matching the npm release.
    Pages,
    /// Versioned npm packages served through jsDelivr.
    JsDelivr,
    /// Explicit `OPENLOGI_ASSETS` or CLI `--base` override.
    Override(String),
}

impl fmt::Display for AssetSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Production => formatter.write_str(PRODUCTION_BASE),
            Self::Pages => formatter.write_str(PAGES_BASE),
            Self::JsDelivr => formatter.write_str(JSDELIVR_CATALOG_BASE),
            Self::Override(base) => formatter.write_str(base),
        }
    }
}

/// A parsed registry and the client pinned to the mirror that supplied it.
pub struct AssetRegistry {
    client: AssetClient,
    index: Index,
    source: AssetSource,
}

impl AssetRegistry {
    /// Load a registry into `dir`.
    ///
    /// An explicit `base` uses that uniform origin. Without an override, the
    /// production domain, versioned Pages alias, and versioned jsDelivr
    /// mirror are probed concurrently; the first complete valid catalog wins.
    pub fn load(base: Option<&str>, dir: &Path) -> Result<Self, AssetError> {
        if let Some(base) = base {
            let client = AssetClient::new(base);
            let index = client.fetch_index_to_dir(dir)?;
            let source = AssetSource::Override(base.to_owned());
            info!(%source, "asset source selected");
            return Ok(Self {
                client,
                index,
                source,
            });
        }
        probe_default_sources(dir)
    }

    /// Client pinned to the selected mirror for the lifetime of this registry.
    #[must_use]
    pub fn client(&self) -> &AssetClient {
        &self.client
    }

    /// Parsed `index.json` returned by the selected mirror.
    #[must_use]
    pub fn index(&self) -> &Index {
        &self.index
    }

    /// Mirror selected by the source probes.
    #[must_use]
    pub fn source(&self) -> &AssetSource {
        &self.source
    }
}

#[derive(Deserialize)]
struct NpmRoutes {
    schema_version: u32,
    package_version: String,
    devices: HashMap<String, String>,
}

struct ProbeSuccess {
    client: AssetClient,
    index_bytes: Vec<u8>,
    index: Index,
}

fn probe_default_sources(dir: &Path) -> Result<AssetRegistry, AssetError> {
    let (sender, receiver) = mpsc::channel();

    for (source, base) in [
        (AssetSource::Production, PRODUCTION_BASE),
        (AssetSource::Pages, PAGES_BASE),
    ] {
        let sender = sender.clone();
        thread::spawn(move || {
            let result = probe_uniform(base);
            if sender.send((source.clone(), result)).is_err() {
                debug!(%source, "asset source probe finished after a winner was selected");
            }
        });
    }
    let jsdelivr_sender = sender.clone();
    thread::spawn(move || {
        let result = probe_jsdelivr();
        if jsdelivr_sender
            .send((AssetSource::JsDelivr, result))
            .is_err()
        {
            debug!("jsDelivr probe finished after a winner was selected");
        }
    });
    drop(sender);

    let mut production_error = None;
    let mut pages_error = None;
    let mut jsdelivr_error = None;
    while let Ok((source, result)) = receiver.recv() {
        match result {
            Ok(probe) => {
                write_replace(&dir.join(INDEX_NAME), &probe.index_bytes)?;
                info!(%source, "asset source selected");
                return Ok(AssetRegistry {
                    client: probe.client,
                    index: probe.index,
                    source,
                });
            }
            Err(error) => {
                warn!(%source, error = ?error, "asset source probe failed");
                match source {
                    AssetSource::Production => production_error = Some(error),
                    AssetSource::Pages => pages_error = Some(error),
                    AssetSource::JsDelivr => jsdelivr_error = Some(error),
                    AssetSource::Override(_) => return Err(AssetError::SourceProbeInterrupted),
                }
            }
        }
        if production_error.is_some() && pages_error.is_some() && jsdelivr_error.is_some() {
            break;
        }
    }
    match (production_error, pages_error, jsdelivr_error) {
        (Some(production), Some(pages), Some(jsdelivr)) => Err(AssetError::SourcesUnavailable {
            production: Box::new(production),
            pages: Box::new(pages),
            jsdelivr: Box::new(jsdelivr),
        }),
        _ => Err(AssetError::SourceProbeInterrupted),
    }
}

fn probe_uniform(base: &str) -> Result<ProbeSuccess, AssetError> {
    let client = AssetClient::new(base);
    let (index_bytes, index) = client.fetch_index_raw()?;
    Ok(ProbeSuccess {
        client,
        index_bytes,
        index,
    })
}

fn probe_jsdelivr() -> Result<ProbeSuccess, AssetError> {
    let catalog_client = AssetClient::new(JSDELIVR_CATALOG_BASE);
    let (index_bytes, index) = catalog_client.fetch_index_raw()?;
    let routes_url = format!("{JSDELIVR_CATALOG_BASE}/{NPM_ROUTES_NAME}");
    let routes_bytes = catalog_client.get_bytes(&routes_url)?;
    let routes: NpmRoutes =
        serde_json::from_slice(&routes_bytes).map_err(|source| AssetError::ParseJson {
            what: "fetched npm-routes.json".to_owned(),
            source,
        })?;
    let client = build_jsdelivr_client(&index, &routes)?;
    Ok(ProbeSuccess {
        client,
        index_bytes,
        index,
    })
}

fn build_jsdelivr_client(index: &Index, routes: &NpmRoutes) -> Result<AssetClient, AssetError> {
    if routes.schema_version != NPM_ROUTES_SCHEMA {
        return Err(AssetError::UnsupportedNpmRoutesSchema {
            expected: NPM_ROUTES_SCHEMA,
            found: routes.schema_version,
        });
    }
    if routes.package_version != ASSET_VERSION {
        return Err(AssetError::NpmRoutesVersionMismatch {
            expected: ASSET_VERSION.to_owned(),
            found: routes.package_version.clone(),
        });
    }
    let mut package_by_asset_path = HashMap::with_capacity(index.devices.len());
    for (depot, entry) in &index.devices {
        let package = routes
            .devices
            .get(depot)
            .ok_or_else(|| AssetError::MissingNpmRoute {
                depot: depot.clone(),
            })?;
        package_by_asset_path.insert(
            entry.asset_path.trim_start_matches('/').to_owned(),
            package.clone(),
        );
    }
    Ok(AssetClient::new_jsdelivr(
        JSDELIVR_CATALOG_BASE,
        JSDELIVR_PACKAGE_ROOT,
        ASSET_VERSION,
        package_by_asset_path,
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::{AssetError, DeviceEntry, Index};

    use super::{ASSET_VERSION, NPM_ROUTES_SCHEMA, NpmRoutes, build_jsdelivr_client};

    fn one_device_index() -> Index {
        Index {
            schema_version: 1,
            devices: HashMap::from([(
                "mx_master_3s".to_owned(),
                DeviceEntry {
                    model_id: "2b043".to_owned(),
                    model_ids: vec!["2b043".to_owned(), "2b034".to_owned()],
                    display_name: "MX Master 3S".to_owned(),
                    kind: "MOUSE".to_owned(),
                    asset_path: "v1/devices/mx_master_3s/".to_owned(),
                    files: Vec::new(),
                },
            )]),
        }
    }

    fn routes_for(package_version: &str) -> NpmRoutes {
        NpmRoutes {
            schema_version: NPM_ROUTES_SCHEMA,
            package_version: package_version.to_owned(),
            devices: HashMap::from([(
                "mx_master_3s".to_owned(),
                "@logi-assets/pointing".to_owned(),
            )]),
        }
    }

    #[test]
    fn npm_route_preserves_the_cloudflare_path_inside_its_shard() {
        let index = one_device_index();
        let routes = routes_for(ASSET_VERSION);

        let url = build_jsdelivr_client(&index, &routes)
            .ok()
            .and_then(|client| {
                client
                    .asset_url("v1/devices/mx_master_3s/", "front_core.png")
                    .ok()
            });

        assert_eq!(
            url.as_deref(),
            Some(
                "https://cdn.jsdelivr.net/npm/@logi-assets/pointing@0.0.1/v1/devices/mx_master_3s/front_core.png"
            )
        );
    }

    #[test]
    fn npm_routes_must_match_the_pinned_asset_version() {
        let index = one_device_index();
        let routes = routes_for("0.0.2");

        assert!(matches!(
            build_jsdelivr_client(&index, &routes),
            Err(AssetError::NpmRoutesVersionMismatch { .. })
        ));
    }

    #[test]
    fn every_catalog_depot_requires_an_npm_route() {
        let index = one_device_index();
        let routes = NpmRoutes {
            schema_version: NPM_ROUTES_SCHEMA,
            package_version: ASSET_VERSION.to_owned(),
            devices: HashMap::new(),
        };

        assert!(matches!(
            build_jsdelivr_client(&index, &routes),
            Err(AssetError::MissingNpmRoute { depot }) if depot == "mx_master_3s"
        ));
    }
}
