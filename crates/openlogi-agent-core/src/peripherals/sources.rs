//! Source transactions and exact-content package admission.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use openlogi_core::{
    config::Config,
    peripheral::{
        CatalogDiagnostic, DescriptorDisclosure, DescriptorId, DriverId, DriverSelection,
        DriverSource, PeripheralError, PluginPackageRecord, PluginSelectionStatus,
    },
};
use openlogi_device_registry::driver::BuiltinDriver;
use openlogi_plugin::{
    descriptor::{Descriptor, Platform},
    manifest::Manifest,
    package::Package,
    runtime::{Compiled, Runtime},
};

use super::catalog::{Catalog, Registration};

/// Package digest → descriptor identity → exact normalized descriptor fingerprint.
pub type Grants = BTreeMap<String, BTreeMap<DescriptorId, String>>;

/// Executable content admitted for selected descriptors of this exact package.
#[derive(Clone)]
pub struct LoadedPlugin {
    /// Installed immutable content.
    pub package: Arc<Package>,
    /// Validated manifest shared by separate stores.
    pub manifest: Arc<Manifest>,
    /// Pulley bytecode reusable across attachments.
    pub component: Compiled,
    /// Approved descriptor fingerprints.
    pub descriptors: BTreeMap<DescriptorId, String>,
}

/// One catalog for compiled facts, descriptor files, and granted components.
#[derive(Clone)]
pub struct Sources {
    /// Accepted source generations.
    pub catalog: Catalog,
    /// Enabled, installed, pinned, and granted implementations.
    pub plugins: BTreeMap<DriverId, LoadedPlugin>,
    /// Last reload's independent source failures.
    pub diagnostics: Vec<CatalogDiagnostic>,
    /// Exact installed content, including disabled packages.
    pub installed: Vec<PluginPackageRecord>,
    external: BTreeMap<String, Descriptor>,
    runtime: Option<Arc<Runtime>>,
}

impl Sources {
    /// Instantiate compiled protocol templates from enumeration facts before probing.
    pub fn observe(
        &mut self,
        endpoints: &[openlogi_hid::peripheral::DiscoveredEndpoint],
        cameras: &[openlogi_core::camera::Camera],
    ) -> Result<(), PeripheralError> {
        let descriptors = super::builtins::observed_descriptors(endpoints, cameras)?;
        let mut entries: BTreeMap<_, _> = self
            .catalog
            .entries()
            .filter(|entry| entry.selection.source == DriverSource::Builtin)
            .map(|entry| (entry.descriptor.id().clone(), entry.clone()))
            .collect();
        for descriptor in descriptors {
            entries.insert(
                descriptor.id().clone(),
                registration(descriptor, DriverSource::Builtin, None),
            );
        }
        self.catalog
            .replace_source(&DriverSource::Builtin, entries.into_values().collect())
    }

    /// Establish compiled registrations without starting an executable runtime.
    pub fn new() -> Result<Self, PeripheralError> {
        let mut catalog = Catalog::default();
        let entries = super::builtins::descriptors()?
            .into_iter()
            .map(|descriptor| registration(descriptor, DriverSource::Builtin, None))
            .collect();
        catalog.replace_source(&DriverSource::Builtin, entries)?;
        Ok(Self {
            catalog,
            plugins: BTreeMap::new(),
            diagnostics: Vec::new(),
            installed: Vec::new(),
            external: BTreeMap::new(),
            runtime: None,
        })
    }

    /// Initialize the interpreter only when executable content is requested.
    pub fn runtime(&mut self) -> Result<Arc<Runtime>, PeripheralError> {
        if let Some(runtime) = &self.runtime {
            return Ok(Arc::clone(runtime));
        }
        let runtime = Runtime::new()?;
        self.runtime = Some(Arc::clone(&runtime));
        Ok(runtime)
    }

    /// Reload independent sources. Invalid edits retain accepted content; missing pins never do.
    pub async fn reload(
        &mut self,
        config: &Config,
        descriptors: &Path,
        packages: &Path,
        grants: &Grants,
    ) {
        self.diagnostics.clear();
        self.installed.clear();
        // Revoke execution before reading edited descriptors. Disabled content may
        // expose a new candidate for review, but cannot validate through cached code.
        let revoked: Vec<_> = self
            .plugins
            .iter()
            .filter(|(id, plugin)| {
                config.plugins.get(*id).is_none_or(|selection| {
                    !selection.enabled || selection.digest != plugin.package.digest()
                })
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in revoked {
            self.plugins.remove(&id);
            if let Err(error) = self
                .catalog
                .replace_source(&DriverSource::Plugin(id.to_string()), Vec::new())
            {
                self.reject(id.to_string(), error, false);
            }
        }
        self.read_descriptors(descriptors);
        self.reload_plugins(config, packages, grants).await;
        for (source, descriptor) in self.external.clone() {
            let driver = descriptor.driver().id.clone();
            let origin = DriverSource::Descriptor(source.clone());
            let mut retained = self
                .catalog
                .entries()
                .any(|entry| entry.selection.source == origin);
            let result = self.bind(&descriptor).and_then(|digest| {
                self.catalog.replace_source(
                    &origin,
                    vec![registration(descriptor, origin.clone(), digest)],
                )
            });
            if let Err(error) = result {
                if !self.plugins.contains_key(&driver)
                    && BuiltinDriver::find(driver.as_ref()).is_none()
                {
                    if let Err(removal) = self.catalog.replace_source(&origin, Vec::new()) {
                        self.reject(source.clone(), removal, false);
                    }
                    retained = false;
                }
                self.reject(source, error, retained);
            }
        }
    }

    /// All current descriptors which this package can implement, before granting access.
    pub fn descriptors_for(&self, package: &Package) -> Result<Vec<Descriptor>, PeripheralError> {
        let mut descriptors = package.descriptors().to_vec();
        descriptors.extend(
            self.external
                .values()
                .filter(|d| d.driver().id == package.manifest().id)
                .cloned(),
        );
        let mut ids = BTreeSet::new();
        for descriptor in &descriptors {
            if !ids.insert(descriptor.id()) {
                return Err(invalid("duplicate descriptor identity"));
            }
            package
                .manifest()
                .validate_descriptor(descriptor)
                .map_err(invalid)?;
        }
        Ok(descriptors)
    }

    fn read_descriptors(&mut self, directory: &Path) {
        let paths = match source_paths(directory, ".device.toml") {
            Ok(paths) => paths,
            Err(error) => {
                self.reject(directory.display().to_string(), error, true);
                return;
            }
        };
        let mut seen = BTreeSet::new();
        for path in paths {
            let source = path.display().to_string();
            seen.insert(source.clone());
            let loaded = (|| {
                let mut bytes = Vec::new();
                fs::File::open(&path)
                    .map_err(invalid)?
                    .take(65537)
                    .read_to_end(&mut bytes)
                    .map_err(invalid)?;
                Descriptor::parse(std::str::from_utf8(&bytes).map_err(invalid)?).map_err(invalid)
            })();
            match loaded {
                Ok(descriptor) => {
                    // Preserve the previous descriptor when parameters or a grant reject an edit.
                    if let Some(Err(error)) = super::builtins::validate(&descriptor) {
                        let retained = self.external.contains_key(&source);
                        self.reject(source, invalid(error), retained);
                        continue;
                    }
                    let origin = DriverSource::Descriptor(source.clone());
                    if self
                        .catalog
                        .entries()
                        .any(|entry| entry.selection.source == origin)
                        && self
                            .catalog
                            .entries()
                            .filter(|entry| entry.selection.source == origin)
                            .any(|entry| {
                                BuiltinDriver::find(entry.selection.driver.as_ref()).is_some()
                                    || self.plugins.contains_key(&entry.selection.driver)
                            })
                        && let Err(error) = self.bind(&descriptor)
                    {
                        self.reject(source, error, true);
                        continue;
                    }
                    self.external.insert(source, descriptor);
                }
                Err(error) => {
                    let retained = self.external.contains_key(&source);
                    self.reject(source, error, retained);
                }
            }
        }
        for removed in self
            .external
            .keys()
            .filter(|key| !seen.contains(*key))
            .cloned()
            .collect::<Vec<_>>()
        {
            self.external.remove(&removed);
            if let Err(error) = self
                .catalog
                .replace_source(&DriverSource::Descriptor(removed.clone()), Vec::new())
            {
                self.reject(removed, error, false);
            }
        }
    }

    fn bind(&self, descriptor: &Descriptor) -> Result<Option<String>, PeripheralError> {
        if let Some(result) = super::builtins::validate(descriptor) {
            result?;
            return Ok(None);
        }
        let plugin = self.plugins.get(&descriptor.driver().id).ok_or_else(|| {
            PeripheralError::DriverUnavailable(descriptor.driver().id.to_string())
        })?;
        plugin
            .manifest
            .validate_descriptor(descriptor)
            .map_err(invalid)?;
        if plugin.descriptors.get(descriptor.id())
            != Some(&descriptor.fingerprint().map_err(invalid)?)
        {
            return Err(PeripheralError::PermissionDenied(
                "descriptor content has not been granted for this package".into(),
            ));
        }
        Ok(Some(plugin.package.digest().to_string()))
    }

    async fn reload_plugins(&mut self, config: &Config, directory: &Path, grants: &Grants) {
        let mut installed = self.installed_packages(directory);
        let mut active = BTreeSet::new();
        for (id, selection) in &config.plugins {
            if !selection.enabled {
                continue;
            }
            let digest = &selection.digest;
            let result = match installed.get(digest) {
                Some(package)
                    if &package.manifest().id == id
                        && package.manifest().settings_schema == selection.settings_schema =>
                {
                    self.admit(package, grants.get(digest)).await
                }
                _ => Err(PeripheralError::DriverUnavailable(format!(
                    "{id}: selected content or settings schema is unavailable"
                ))),
            };
            match result {
                Ok(component) => {
                    let Some(package) = installed.remove(digest) else {
                        continue;
                    };
                    let approved = grants.get(digest).cloned().unwrap_or_default();
                    // The source identifies the implementation; the selection pins its content.
                    // Keeping the source stable makes upgrades an atomic catalog replacement.
                    let source = DriverSource::Plugin(id.to_string());
                    let entries = package
                        .descriptors()
                        .iter()
                        .filter(|d| approved.contains_key(d.id()))
                        .map(|d| registration(d.clone(), source.clone(), Some(digest.clone())))
                        .collect();
                    let disclosure = match self.disclosure(
                        &package,
                        PluginSelectionStatus::Enabled,
                        &approved,
                    ) {
                        Ok(disclosure) => disclosure,
                        Err(error) => {
                            self.reject(digest.clone(), error, false);
                            continue;
                        }
                    };
                    match self.catalog.replace_source(&source, entries) {
                        Ok(()) => {
                            self.installed.push(disclosure);
                            self.plugins.insert(
                                id.clone(),
                                LoadedPlugin {
                                    manifest: Arc::new(package.manifest().clone()),
                                    package: Arc::new(package),
                                    component,
                                    descriptors: approved,
                                },
                            );
                            active.insert(id.clone());
                        }
                        Err(error) => self.reject(digest.clone(), error, false),
                    }
                }
                Err(error) => self.reject(digest.clone(), error, false),
            }
        }
        for package in installed.values() {
            let selection = config
                .plugins
                .get(&package.manifest().id)
                .filter(|s| s.digest == package.digest());
            match self.disclosure(
                package,
                match selection {
                    Some(selection) if selection.enabled => PluginSelectionStatus::Enabled,
                    Some(_) => PluginSelectionStatus::Disabled,
                    None => PluginSelectionStatus::Unselected,
                },
                grants.get(package.digest()).unwrap_or(&BTreeMap::new()),
            ) {
                Ok(record) => self.installed.push(record),
                Err(error) => self.reject(package.digest().into(), error, false),
            }
        }
        for (id, digest) in self
            .plugins
            .iter()
            .filter(|(id, _)| !active.contains(*id))
            .map(|(id, p)| (id.clone(), p.package.digest().to_string()))
            .collect::<Vec<_>>()
        {
            self.plugins.remove(&id);
            if let Err(error) = self
                .catalog
                .replace_source(&DriverSource::Plugin(id.to_string()), Vec::new())
            {
                self.reject(digest, error, false);
            }
        }
    }

    fn installed_packages(&mut self, directory: &Path) -> BTreeMap<String, Package> {
        let paths = match source_paths(directory, "") {
            Ok(paths) => paths,
            Err(error) => {
                self.reject(directory.display().to_string(), error, false);
                Vec::new()
            }
        };
        let mut installed = BTreeMap::new();
        for path in paths {
            let digest = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if digest.starts_with('.') {
                continue;
            }
            match Package::installed(directory, &digest) {
                Ok(package) => {
                    installed.insert(digest, package);
                }
                Err(error) => self.reject(digest, invalid(error), false),
            }
        }
        installed
    }

    async fn admit(
        &mut self,
        package: &Package,
        approved: Option<&BTreeMap<DescriptorId, String>>,
    ) -> Result<Compiled, PeripheralError> {
        let manifest = package.manifest();
        let approved = approved.filter(|d| !d.is_empty()).ok_or_else(|| {
            PeripheralError::PermissionDenied("package has no descriptor grants".into())
        })?;
        if Platform::current().is_none_or(|platform| !manifest.platforms.contains(&platform)) {
            return Err(PeripheralError::Unsupported("plugin platform".into()));
        }
        let descriptors = self.descriptors_for(package)?;
        for (id, fingerprint) in approved {
            let descriptor = descriptors.iter().find(|d| d.id() == id).ok_or_else(|| {
                PeripheralError::PermissionDenied(format!("granted descriptor {id} is missing"))
            })?;
            if descriptor.fingerprint().map_err(invalid)? != *fingerprint {
                return Err(PeripheralError::PermissionDenied(format!(
                    "descriptor {id} changed since approval"
                )));
            }
        }
        if let Some(old) = self.plugins.get(&manifest.id)
            && old.package.digest() == package.digest()
            && &old.descriptors == approved
        {
            return Ok(old.component.clone());
        }
        self.runtime()?.compile(package.component().to_vec()).await
    }

    fn disclosure(
        &self,
        package: &Package,
        selection: PluginSelectionStatus,
        granted: &BTreeMap<DescriptorId, String>,
    ) -> Result<PluginPackageRecord, PeripheralError> {
        let mut record = package_record(package, selection)?;
        for descriptor in self
            .external
            .values()
            .filter(|d| d.driver().id == package.manifest().id)
        {
            record
                .descriptors
                .insert(descriptor.id().clone(), disclose_descriptor(descriptor)?);
        }
        for (id, details) in &mut record.descriptors {
            details.granted = granted.get(id) == Some(&details.fingerprint);
        }
        Ok(record)
    }

    fn reject(&mut self, source: String, error: PeripheralError, retained: bool) {
        self.diagnostics.push(CatalogDiagnostic {
            source,
            error,
            retained,
        });
    }
}

/// Disclosure of the exact bytes reviewed for enablement.
pub fn package_record(
    package: &Package,
    selection: PluginSelectionStatus,
) -> Result<PluginPackageRecord, PeripheralError> {
    Ok(PluginPackageRecord {
        driver: package.manifest().id.clone(),
        version: package.manifest().version.to_string(),
        digest: package.digest().into(),
        descriptors: package
            .descriptors()
            .iter()
            .map(|d| Ok((d.id().clone(), disclose_descriptor(d)?)))
            .collect::<Result<_, PeripheralError>>()?,
        permissions: package
            .manifest()
            .permissions
            .iter()
            .map(openlogi_plugin::manifest::Permission::disclosure)
            .collect(),
        selection,
        active: false,
        rollback_available: false,
    })
}

fn disclose_descriptor(descriptor: &Descriptor) -> Result<DescriptorDisclosure, PeripheralError> {
    let matches = descriptor
        .selectors()
        .iter()
        .map(|s| {
            format!(
                "{}: {:?} {:04x}:{:04x}, usage={:?}/{:?}, interface={:?}, report={:?}",
                s.role,
                s.transport,
                s.vendor_id,
                s.product_id,
                s.usage_page,
                s.usage,
                s.interface,
                s.report_id
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    Ok(DescriptorDisclosure {
        fingerprint: descriptor.fingerprint().map_err(invalid)?,
        matching: format!("{} ({}) — {matches}", descriptor.name(), descriptor.model()),
        granted: false,
    })
}

fn registration(
    descriptor: Descriptor,
    source: DriverSource,
    digest: Option<String>,
) -> Registration {
    let selection = DriverSelection {
        descriptor: descriptor.id().clone(),
        driver: descriptor.driver().id.clone(),
        digest,
        source,
    };
    Registration {
        descriptor,
        selection,
    }
}

fn source_paths(directory: &Path, suffix: &str) -> Result<Vec<PathBuf>, PeripheralError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(invalid(error)),
    };
    let mut paths = Vec::new();
    for (count, entry) in entries.enumerate() {
        if count >= 256 {
            return Err(PeripheralError::ResourceLimit(
                "more than 256 extension sources".into(),
            ));
        }
        let entry = entry.map_err(invalid)?;
        if entry.file_type().map_err(invalid)?.is_symlink() {
            return Err(invalid("extension source is a symlink"));
        }
        if suffix.is_empty() || entry.file_name().to_string_lossy().ends_with(suffix) {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn invalid(error: impl std::fmt::Display) -> PeripheralError {
    PeripheralError::InvalidSettings(error.to_string())
}
