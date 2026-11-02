//! Opt-in filesystem boundary for strictly verified fixture assets.

use std::collections::BTreeMap;
use std::fs::{self, DirEntry};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::{DeviceProfile, FixtureError, FixtureManifest, FixtureVerificationError, HidCassette};

const MANIFEST_FILE: &str = "manifest.json";
const PROFILE_FILE: &str = "profile.json";
const CASES_DIRECTORY: &str = "cases";

/// Failure to read or verify a fixture. Sources retain their original error types.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// A specimen within a corpus failed loading or verification.
    #[error("invalid fixture {path}: {source}")]
    Specimen {
        /// Directory of the rejected specimen.
        path: PathBuf,
        /// Original loading or verification failure.
        source: Box<LoadError>,
    },
    /// An asset could not be accessed.
    #[error("fixture structure verification failed: {path}: {source}")]
    Io {
        /// Asset whose access failed.
        path: PathBuf,
        /// Original filesystem failure.
        source: std::io::Error,
    },
    /// An asset is not valid JSON for its schema.
    #[error("schema verification failed while parsing {path}: {source}")]
    Json {
        /// Asset whose JSON is invalid.
        path: PathBuf,
        /// Original parser failure.
        source: serde_json::Error,
    },
    /// The directory layout does not match the manifest.
    #[error("fixture structure verification failed: {0}")]
    Structure(String),
    /// A typed asset violates its schema.
    #[error("schema verification failed: {0}")]
    Schema(#[from] FixtureError),
    /// The assets disagree or contain invalid protocol or identity evidence.
    #[error(transparent)]
    Verification(#[from] FixtureVerificationError),
}

type Result<T> = std::result::Result<T, LoadError>;

/// A complete fixture whose schema, privacy, and relationships have been verified.
#[derive(Debug)]
pub struct Fixture {
    manifest: FixtureManifest,
    profile: DeviceProfile,
    cassettes: Vec<HidCassette>,
}

impl Fixture {
    /// Load the exact declared assets and reject symlinks, extras, and invalid evidence.
    pub fn load(directory: &Path) -> Result<Self> {
        require_directory(directory)?;
        let mut manifest_path = None;
        let mut profile_path = None;
        let mut cases_path = None;
        for entry in entries(directory)? {
            match entry_name(&entry)?.as_str() {
                MANIFEST_FILE => {
                    require_regular_file(&entry)?;
                    manifest_path = Some(entry.path());
                }
                PROFILE_FILE => {
                    require_regular_file(&entry)?;
                    profile_path = Some(entry.path());
                }
                CASES_DIRECTORY => {
                    require_directory(&entry.path())?;
                    cases_path = Some(entry.path());
                }
                name => {
                    return Err(LoadError::Structure(format!(
                        "unexpected entry {name:?} in {}",
                        directory.display()
                    )));
                }
            }
        }
        let manifest: FixtureManifest = read_json(&manifest_path.ok_or_else(|| {
            LoadError::Structure("fixture directory has no manifest.json".into())
        })?)?;
        manifest.validate()?;
        if directory.file_name().and_then(|name| name.to_str()) != Some(manifest.id.as_str()) {
            return Err(LoadError::Structure(format!(
                "fixture directory name must equal manifest id {:?}",
                manifest.id
            )));
        }
        let profile: DeviceProfile = read_json(&profile_path.ok_or_else(|| {
            LoadError::Structure("fixture directory has no profile.json".into())
        })?)?;
        profile.validate()?;
        let cassettes = load_cassettes(cases_path.as_deref(), &manifest)?;
        manifest.verify_detailed(&profile, &cassettes)?;
        Ok(Self {
            manifest,
            profile,
            cassettes,
        })
    }

    /// The verified specimen manifest.
    #[must_use]
    pub fn manifest(&self) -> &FixtureManifest {
        &self.manifest
    }

    /// The independently reviewable semantic profile.
    #[must_use]
    pub fn profile(&self) -> &DeviceProfile {
        &self.profile
    }

    /// The complete declared cassette set, ordered by filename.
    #[must_use]
    pub fn cassettes(&self) -> &[HidCassette] {
        &self.cassettes
    }
}

/// Discover and strictly load every specimen in a corpus directory.
///
/// Missing and empty corpora fail so a test cannot silently lose all coverage.
pub fn load_corpus(directory: &Path) -> Result<Vec<Fixture>> {
    require_directory(directory)?;
    let entries = entries(directory)?;
    if entries.is_empty() {
        return Err(LoadError::Structure(format!(
            "fixture corpus {} is empty",
            directory.display()
        )));
    }
    entries
        .into_iter()
        .map(|entry| {
            let path = entry.path();
            Fixture::load(&path).map_err(|source| LoadError::Specimen {
                path,
                source: Box::new(source),
            })
        })
        .collect()
}

/// Load contributed specimens when built inside the repository workspace.
///
/// Published packages have no workspace manifest two levels above this crate.
/// In a workspace, an absent or empty corpus is an error.
pub fn repository_corpus() -> Result<Option<Vec<Fixture>>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = root.join("Cargo.toml");
    if !manifest.try_exists().map_err(|source| LoadError::Io {
        path: manifest,
        source,
    })? {
        eprintln!("repository corpus unavailable in a published package");
        return Ok(None);
    }
    load_corpus(&root.join("fixtures/devices")).map(Some)
}

fn load_cassettes(
    directory: Option<&Path>,
    manifest: &FixtureManifest,
) -> Result<Vec<HidCassette>> {
    let mut expected = BTreeMap::new();
    for case in &manifest.cases {
        let name = &case.name;
        if matches!(name.as_str(), "." | "..") || name.contains('/') || name.contains('\\') {
            return Err(LoadError::Structure(format!(
                "fixture case name {name:?} is not a safe file name"
            )));
        }
        if expected
            .insert(format!("{name}.json"), name.as_str())
            .is_some()
        {
            return Err(LoadError::Structure(
                "fixture case names map to the same file".into(),
            ));
        }
    }
    if expected.is_empty() {
        if directory.is_some() {
            return Err(LoadError::Structure(
                "profile-only fixture has an undeclared cases directory".into(),
            ));
        }
        return Ok(Vec::new());
    }
    let directory = directory.ok_or_else(|| {
        LoadError::Structure("manifest declares cases but the cases directory is missing".into())
    })?;
    let mut found = BTreeMap::new();
    for entry in entries(directory)? {
        require_regular_file(&entry)?;
        let name = entry_name(&entry)?;
        let case_name = expected.get(&name).ok_or_else(|| {
            LoadError::Structure(format!("undeclared fixture case file {name:?}"))
        })?;
        let cassette: HidCassette = read_json(&entry.path())?;
        if cassette.name != *case_name {
            return Err(LoadError::Structure(format!(
                "cassette file {name:?} contains case {:?}",
                cassette.name
            )));
        }
        found.insert(name, cassette);
    }
    if let Some(missing) = expected.keys().find(|name| !found.contains_key(*name)) {
        return Err(LoadError::Structure(format!(
            "missing declared fixture case file {missing:?}"
        )));
    }
    Ok(found.into_values().collect())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).map_err(|source| LoadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| LoadError::Json {
        path: path.to_path_buf(),
        source,
    })
}

fn entries(directory: &Path) -> Result<Vec<DirEntry>> {
    let mut entries = fs::read_dir(directory)
        .and_then(Iterator::collect::<std::io::Result<Vec<_>>>)
        .map_err(|source| LoadError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(DirEntry::file_name);
    Ok(entries)
}

fn require_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|source| LoadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(LoadError::Structure(format!(
            "{} must be a non-symlink directory",
            path.display()
        )));
    }
    Ok(())
}

fn require_regular_file(entry: &DirEntry) -> Result<()> {
    let path = entry.path();
    let metadata = fs::symlink_metadata(&path).map_err(|source| LoadError::Io { path, source })?;
    if !metadata.is_file() {
        return Err(LoadError::Structure(
            "fixture asset must be a non-symlink regular file".into(),
        ));
    }
    Ok(())
}

fn entry_name(entry: &DirEntry) -> Result<String> {
    entry
        .file_name()
        .into_string()
        .map_err(|_| LoadError::Structure("fixture paths must be valid UTF-8".into()))
}

#[cfg(test)]
mod tests;
