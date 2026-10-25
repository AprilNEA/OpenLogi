//! Built-in asset mirror discovery and npm shard routing.
//!
//! A synchronization run races the production custom domain, the Cloudflare
//! Pages alias of this build's asset release line, and the newest jsDelivr
//! npm release of that line. The first source with a complete valid catalog
//! supplies both `index.json` and every subsequent file URL for that run, so
//! caches never mix mirrors mid-sync.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::error::AssetError;
use crate::http::{AssetClient, write_replace};
use crate::index::{INDEX_NAME, Index};

/// Mutable production endpoint behind the OpenLogi custom domain.
const PRODUCTION_BASE: &str = "https://assets.openlogi.org";

/// Asset release line this OpenLogi build reads: every `0.1.<patch>` release.
///
/// The asset repository releases each catalog change as the next patch of its
/// line, so a build follows new devices without an app update. A new minor
/// release may change the layout, so a build never leaves its line.
const ASSET_LINE: &str = "0.1";

/// jsDelivr prefix shared by every npm asset package.
const JSDELIVR_PACKAGE_ROOT: &str = "https://cdn.jsdelivr.net/npm";

/// jsDelivr data API root. Its range answer is at most about an hour old
/// (`max-age=300`, `stale-while-revalidate=3600`); the CDN caches a range for
/// up to 12 hours (`s-maxage=43200`).
const JSDELIVR_DATA_ROOT: &str = "https://data.jsdelivr.com/v1/packages/npm";

/// How long the jsDelivr probe waits for the data API after the CDN range
/// has answered. The lookup can only move the probe to a newer release, so a
/// slow or blocked API costs at most this much.
const JSDELIVR_RESOLVE_WAIT: Duration = Duration::from_secs(1);

/// npm package that carries `index.json` and the shard routes.
const CATALOG_PACKAGE: &str = "@logi-assets/catalog";

/// Filename and schema of the depot-to-package routing catalog.
const NPM_ROUTES_NAME: &str = "npm-routes.json";
const NPM_ROUTES_SCHEMA: u32 = 1;

/// Cloudflare Pages alias of [`ASSET_LINE`] (`v0-1`). The asset release
/// workflow moves it to the newest release of the line.
fn pages_base() -> String {
    format!(
        "https://v{}.openlogi-assets.pages.dev",
        ASSET_LINE.replace('.', "-")
    )
}

/// jsDelivr catalog URL for `version`: one exact release, or the
/// [`ASSET_LINE`] range that jsDelivr resolves to its newest release.
fn jsdelivr_catalog_base(version: &str) -> String {
    format!("{JSDELIVR_PACKAGE_ROOT}/{CATALOG_PACKAGE}@{version}")
}

/// Patch number of `version` when it is a `<ASSET_LINE>.<patch>` release
/// this build can read, or `None` for any other version.
fn asset_line_patch(version: &str) -> Option<u64> {
    let patch = version
        .strip_prefix(ASSET_LINE)?
        .strip_prefix('.')
        .filter(|patch| !patch.is_empty() && patch.bytes().all(|byte| byte.is_ascii_digit()))?;
    patch.parse().ok()
}

/// Asset endpoint selected for one synchronization run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssetSource {
    /// Mutable production custom domain.
    Production,
    /// Cloudflare Pages alias of this build's asset release line.
    Pages,
    /// Newest npm release of this build's asset release line, served through
    /// jsDelivr.
    JsDelivr,
    /// Explicit `OPENLOGI_ASSETS` or CLI `--base` override.
    Override(String),
}

#[derive(Clone, Copy, Debug)]
enum BuiltInSource {
    Production,
    Pages,
    JsDelivr,
}

impl From<BuiltInSource> for AssetSource {
    fn from(source: BuiltInSource) -> Self {
        match source {
            BuiltInSource::Production => Self::Production,
            BuiltInSource::Pages => Self::Pages,
            BuiltInSource::JsDelivr => Self::JsDelivr,
        }
    }
}

impl fmt::Display for BuiltInSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        AssetSource::from(*self).fmt(formatter)
    }
}

impl fmt::Display for AssetSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Production => formatter.write_str(PRODUCTION_BASE),
            Self::Pages => formatter.write_str(&pages_base()),
            Self::JsDelivr => formatter.write_str(&jsdelivr_catalog_base(ASSET_LINE)),
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
    /// production domain, the release-line Pages alias, and the release-line
    /// jsDelivr mirror are probed concurrently; the first complete valid
    /// catalog wins.
    pub fn load(base: Option<&str>, dir: &Path) -> Result<Self, AssetError> {
        Self::load_source(base.map(|base| AssetSource::Override(base.to_owned())), dir)
    }

    /// Load a registry from one selected built-in source, or race all built-in
    /// sources when `source` is `None`.
    ///
    /// Unlike passing the jsDelivr catalog URL as a uniform override to
    /// [`Self::load`], selecting [`AssetSource::JsDelivr`] also loads and
    /// validates its npm shard routes.
    pub fn load_source(source: Option<AssetSource>, dir: &Path) -> Result<Self, AssetError> {
        match source {
            Some(source) => load_selected_source(source, dir),
            None => probe_default_sources(dir),
        }
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

/// `npm-routes.json` of one release in [`ASSET_LINE`].
///
/// Only [`TryFrom<NpmRoutes>`] builds it, so its version is a validated
/// `<line>.<patch>` that is safe to put in a URL.
struct NpmRelease {
    version: String,
    patch: u64,
    devices: HashMap<String, String>,
}

impl TryFrom<NpmRoutes> for NpmRelease {
    type Error = AssetError;

    fn try_from(routes: NpmRoutes) -> Result<Self, AssetError> {
        if routes.schema_version != NPM_ROUTES_SCHEMA {
            return Err(AssetError::UnsupportedNpmRoutesSchema {
                expected: NPM_ROUTES_SCHEMA,
                found: routes.schema_version,
            });
        }
        let Some(patch) = asset_line_patch(&routes.package_version) else {
            return Err(AssetError::NpmRoutesVersionMismatch {
                expected: format!("{ASSET_LINE}.x"),
                found: routes.package_version,
            });
        };
        Ok(Self {
            version: routes.package_version,
            patch,
            devices: routes.devices,
        })
    }
}

impl NpmRelease {
    /// jsDelivr catalog URL of exactly this release.
    fn catalog_base(&self) -> String {
        jsdelivr_catalog_base(&self.version)
    }

    /// Whether `version` is a release of [`ASSET_LINE`] newer than this one.
    fn is_older_than(&self, version: &str) -> bool {
        asset_line_patch(version).is_some_and(|patch| patch > self.patch)
    }

    /// Client that fetches every depot in `index` from this release's shards.
    fn client(&self, index: &Index) -> Result<AssetClient, AssetError> {
        let mut package_by_asset_path = HashMap::with_capacity(index.devices.len());
        for (depot, entry) in &index.devices {
            let package = self
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
            &self.catalog_base(),
            JSDELIVR_PACKAGE_ROOT,
            &self.version,
            package_by_asset_path,
        ))
    }
}

struct ProbeSuccess {
    client: AssetClient,
    index_bytes: Vec<u8>,
    index: Index,
}

fn load_selected_source(source: AssetSource, dir: &Path) -> Result<AssetRegistry, AssetError> {
    let probe = match &source {
        AssetSource::Production => probe_uniform(PRODUCTION_BASE),
        AssetSource::Pages => probe_uniform(&pages_base()),
        AssetSource::JsDelivr => probe_jsdelivr(),
        AssetSource::Override(base) => probe_uniform(base),
    }?;
    registry_from_probe(source, probe, dir)
}

fn registry_from_probe(
    source: AssetSource,
    probe: ProbeSuccess,
    dir: &Path,
) -> Result<AssetRegistry, AssetError> {
    write_replace(&dir.join(INDEX_NAME), &probe.index_bytes)?;
    info!(%source, "asset source selected");
    Ok(AssetRegistry {
        client: probe.client,
        index: probe.index,
        source,
    })
}

fn probe_default_sources(dir: &Path) -> Result<AssetRegistry, AssetError> {
    let (sender, receiver) = mpsc::channel();

    for (source, base) in [
        (BuiltInSource::Production, PRODUCTION_BASE.to_owned()),
        (BuiltInSource::Pages, pages_base()),
    ] {
        let sender = sender.clone();
        thread::spawn(move || {
            let result = probe_uniform(&base);
            if sender.send((source, result)).is_err() {
                debug!(%source, "asset source probe finished after a winner was selected");
            }
        });
    }
    let jsdelivr_sender = sender.clone();
    thread::spawn(move || {
        let result = probe_jsdelivr();
        if jsdelivr_sender
            .send((BuiltInSource::JsDelivr, result))
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
                // A local persistence failure is independent of the selected mirror.
                return registry_from_probe(source.into(), probe, dir);
            }
            Err(error) => {
                warn!(%source, error = ?error, "asset source probe failed");
                match source {
                    BuiltInSource::Production => production_error = Some(error),
                    BuiltInSource::Pages => pages_error = Some(error),
                    BuiltInSource::JsDelivr => jsdelivr_error = Some(error),
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

/// Probe the newest jsDelivr release of [`ASSET_LINE`].
///
/// The CDN resolves the `catalog@<line>` range but caches the answer for up
/// to 12 hours. The data API answer is at most about an hour old, so a
/// background thread asks it while the range loads. The probe waits for that
/// answer at most [`JSDELIVR_RESOLVE_WAIT`] and uses it only when it names a
/// newer release whose routes, index, and shard routing all load; otherwise
/// the range release stands.
///
/// The chosen `npm-routes.json` names the exact release, and `index.json` and
/// every shard file come from that release, so one run never mixes releases.
fn probe_jsdelivr() -> Result<ProbeSuccess, AssetError> {
    let (sender, newest) = mpsc::channel();
    thread::spawn(move || {
        if sender.send(resolve_newest_release()).is_err() {
            debug!("jsDelivr data API answered after the probe stopped waiting");
        }
    });
    let range = fetch_npm_release(&jsdelivr_catalog_base(ASSET_LINE))?;
    if let Ok(Some(version)) = newest.recv_timeout(JSDELIVR_RESOLVE_WAIT)
        && range.is_older_than(&version)
    {
        match fetch_npm_release(&jsdelivr_catalog_base(&version))
            .and_then(|newer| probe_npm_release(&newer))
        {
            Ok(probe) => return Ok(probe),
            Err(error) => {
                warn!(%version, error = %error, "newest jsDelivr asset release unavailable; using the cached range");
            }
        }
    }
    probe_npm_release(&range)
}

/// Load `index.json` of exactly `release` and route every depot to its shard.
fn probe_npm_release(release: &NpmRelease) -> Result<ProbeSuccess, AssetError> {
    debug!(version = %release.version, "jsDelivr asset release resolved");
    let (index_bytes, index) = AssetClient::new(&release.catalog_base()).fetch_index_raw()?;
    let client = release.client(&index)?;
    Ok(ProbeSuccess {
        client,
        index_bytes,
        index,
    })
}

/// `npm-routes.json` under `catalog_base`, validated as a release of
/// [`ASSET_LINE`].
fn fetch_npm_release(catalog_base: &str) -> Result<NpmRelease, AssetError> {
    let routes_bytes =
        AssetClient::new(catalog_base).get_bytes(&format!("{catalog_base}/{NPM_ROUTES_NAME}"))?;
    let routes: NpmRoutes =
        serde_json::from_slice(&routes_bytes).map_err(|source| AssetError::ParseJson {
            what: "fetched npm-routes.json".to_owned(),
            source,
        })?;
    NpmRelease::try_from(routes)
}

/// Newest release of [`ASSET_LINE`] according to the jsDelivr data API, or
/// `None` when the API fails or names no release in the line.
fn resolve_newest_release() -> Option<String> {
    let url = format!("{JSDELIVR_DATA_ROOT}/{CATALOG_PACKAGE}/resolved?specifier={ASSET_LINE}");
    match AssetClient::new(JSDELIVR_DATA_ROOT).get_bytes(&url) {
        Ok(bytes) => newest_release_in(&bytes),
        Err(error) => {
            debug!(error = %error, "jsDelivr data API lookup failed");
            None
        }
    }
}

/// Data API answer for one npm version range.
#[derive(Deserialize)]
struct ResolvedVersion {
    /// Newest matching version; `null` when nothing matches.
    version: Option<String>,
}

/// Release named by a data API `body`, when it belongs to [`ASSET_LINE`].
fn newest_release_in(body: &[u8]) -> Option<String> {
    let version = serde_json::from_slice::<ResolvedVersion>(body)
        .ok()?
        .version?;
    asset_line_patch(&version).map(|_| version)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::{AssetError, DeviceEntry, Index};

    use super::{AssetSource, NPM_ROUTES_SCHEMA, NpmRelease, NpmRoutes, newest_release_in};

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

    /// #1995: a pinned release went stale while production kept adding
    /// devices, so the fallbacks must track the release line instead.
    #[test]
    fn built_in_fallbacks_follow_the_asset_release_line() {
        assert_eq!(
            AssetSource::Pages.to_string(),
            "https://v0-1.openlogi-assets.pages.dev"
        );
        assert_eq!(
            AssetSource::JsDelivr.to_string(),
            "https://cdn.jsdelivr.net/npm/@logi-assets/catalog@0.1"
        );
    }

    #[test]
    fn npm_release_pins_the_catalog_and_shards_to_its_exact_version() {
        let index = one_device_index();
        let release = NpmRelease::try_from(routes_for("0.1.17")).ok();

        let catalog = release.as_ref().map(NpmRelease::catalog_base);
        let url = release
            .and_then(|release| release.client(&index).ok())
            .and_then(|client| {
                client
                    .asset_url("v1/devices/mx_master_3s/", "front_core.png")
                    .ok()
            });

        assert_eq!(
            catalog.as_deref(),
            Some("https://cdn.jsdelivr.net/npm/@logi-assets/catalog@0.1.17")
        );
        assert_eq!(
            url.as_deref(),
            Some(
                "https://cdn.jsdelivr.net/npm/@logi-assets/pointing@0.1.17/v1/devices/mx_master_3s/front_core.png"
            )
        );
    }

    #[test]
    fn npm_routes_outside_the_asset_line_are_rejected() {
        for version in [
            "0.2.0",
            "1.1.0",
            "0.10.1",
            "0.1",
            "0.1.",
            "0.1.0-rc.1",
            "0.1.1/../2",
        ] {
            assert!(
                matches!(
                    NpmRelease::try_from(routes_for(version)),
                    Err(AssetError::NpmRoutesVersionMismatch { found, .. }) if found == version
                ),
                "{version} must be rejected"
            );
        }
    }

    #[test]
    fn npm_routes_with_an_unknown_schema_are_rejected() {
        let routes = NpmRoutes {
            schema_version: NPM_ROUTES_SCHEMA + 1,
            ..routes_for("0.1.1")
        };

        assert!(matches!(
            NpmRelease::try_from(routes),
            Err(AssetError::UnsupportedNpmRoutesSchema { .. })
        ));
    }

    #[test]
    fn every_catalog_depot_requires_an_npm_route() {
        let index = one_device_index();
        let routes = NpmRoutes {
            devices: HashMap::new(),
            ..routes_for("0.1.1")
        };

        assert!(matches!(
            NpmRelease::try_from(routes).and_then(|release| release.client(&index)),
            Err(AssetError::MissingNpmRoute { depot }) if depot == "mx_master_3s"
        ));
    }

    #[test]
    fn data_api_answers_outside_the_asset_line_are_ignored() {
        let answer = |body: &str| newest_release_in(body.as_bytes());

        assert_eq!(
            answer(r#"{"type":"npm","name":"@logi-assets/catalog","version":"0.1.4","links":{}}"#)
                .as_deref(),
            Some("0.1.4")
        );
        assert_eq!(answer(r#"{"version":null}"#), None);
        assert_eq!(answer(r#"{"version":"0.2.0"}"#), None);
        assert_eq!(answer(r#"{"version":"0.1.4/../../x"}"#), None);
        assert_eq!(answer("<html>rate limited</html>"), None);
    }

    #[test]
    fn only_a_newer_patch_replaces_the_range_release() {
        let release = NpmRelease::try_from(routes_for("0.1.9")).ok();
        let replaces = |version: &str| {
            release
                .as_ref()
                .is_some_and(|release| release.is_older_than(version))
        };

        assert!(replaces("0.1.10"));
        assert!(!replaces("0.1.9"));
        assert!(!replaces("0.1.8"));
        assert!(!replaces("0.2.0"));
    }
}
