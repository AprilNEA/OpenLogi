//! Strict on-disk fixture corpus verification.

use std::collections::BTreeMap;
use std::fs::{self, DirEntry};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use openlogi_device::fixture::{DeviceProfile, FixtureManifest, HidCassette};
use serde::de::DeserializeOwned;

const MANIFEST_FILE: &str = "manifest.json";
const PROFILE_FILE: &str = "profile.json";
const CASES_DIRECTORY: &str = "cases";

/// Arguments for strict fixture corpus verification.
#[derive(Args, Debug)]
pub struct VerifyArgs {
    /// Fixture directory containing manifest.json, profile.json, and declared cases.
    #[arg(value_name = "DIRECTORY")]
    pub directory: PathBuf,
}

pub fn run(args: &VerifyArgs) -> Result<()> {
    let fixture = LoadedFixture::load(&args.directory)?;
    fixture.verify()?;
    println!("verified fixture {}", fixture.manifest.id);
    println!(
        "  schema: manifest, profile, and {} cassette(s) are valid",
        fixture.cassettes.len()
    );
    println!("  privacy: exact synthetic identity ledger matched");
    println!("  relationships: profile and every declared case matched");
    println!(
        "  replay: {} declared cassette(s) passed framing and supported protocol validation",
        fixture.cassettes.len()
    );
    println!("  hardware: not exercised; semantic correctness is not established");
    Ok(())
}

#[derive(Debug)]
struct LoadedFixture {
    manifest: FixtureManifest,
    profile: DeviceProfile,
    cassettes: Vec<HidCassette>,
}

impl LoadedFixture {
    fn load(directory: &Path) -> Result<Self> {
        let layout = FixtureLayout::inspect(directory)?;
        let manifest: FixtureManifest = read_json(&layout.manifest, "fixture manifest")?;
        manifest
            .validate()
            .context("schema verification failed for fixture manifest")?;
        require_directory_id(directory, &manifest.id)?;

        let profile: DeviceProfile = read_json(&layout.profile, "device profile")?;
        profile
            .validate()
            .context("schema verification failed for device profile")?;
        let cassettes = load_cassettes(layout.cases.as_deref(), &manifest)?;

        Ok(Self {
            manifest,
            profile,
            cassettes,
        })
    }

    fn verify(&self) -> Result<()> {
        self.manifest
            .verify_detailed(&self.profile, &self.cassettes)
            .map_err(Into::into)
    }
}

struct FixtureLayout {
    manifest: PathBuf,
    profile: PathBuf,
    cases: Option<PathBuf>,
}

impl FixtureLayout {
    fn inspect(directory: &Path) -> Result<Self> {
        require_directory(directory, "fixture directory")?;
        let mut manifest = None;
        let mut profile = None;
        let mut cases = None;
        for entry in read_directory(directory, "fixture directory")? {
            let name = entry_name(&entry)?;
            match name.as_str() {
                MANIFEST_FILE => {
                    require_regular_file(&entry, "fixture manifest")?;
                    manifest = Some(entry.path());
                }
                PROFILE_FILE => {
                    require_regular_file(&entry, "device profile")?;
                    profile = Some(entry.path());
                }
                CASES_DIRECTORY => {
                    require_entry_directory(&entry, "fixture cases directory")?;
                    cases = Some(entry.path());
                }
                _ => bail!(
                    "fixture structure verification failed: unexpected entry {name:?} in {}",
                    directory.display()
                ),
            }
        }
        Ok(Self {
            manifest: manifest.context(
                "fixture structure verification failed: fixture directory has no manifest.json",
            )?,
            profile: profile.context(
                "fixture structure verification failed: fixture directory has no profile.json",
            )?,
            cases,
        })
    }
}

fn load_cassettes(
    cases_directory: Option<&Path>,
    manifest: &FixtureManifest,
) -> Result<Vec<HidCassette>> {
    let mut expected = BTreeMap::new();
    for case in &manifest.cases {
        let file_name = case_file_name(&case.name)?;
        if expected.insert(file_name, case.name.as_str()).is_some() {
            bail!("relationship verification failed: fixture case names map to the same file");
        }
    }

    if expected.is_empty() {
        if cases_directory.is_some() {
            bail!(
                "relationship verification failed: profile-only fixture has an undeclared cases directory"
            );
        }
        return Ok(Vec::new());
    }
    let directory = cases_directory.context(
        "relationship verification failed: manifest declares cases but the cases directory is missing",
    )?;

    let mut found = BTreeMap::new();
    for entry in read_directory(directory, "fixture cases directory")? {
        require_regular_file(&entry, "fixture cassette")?;
        let file_name = entry_name(&entry)?;
        let Some(case_name) = expected.get(&file_name) else {
            bail!("relationship verification failed: undeclared fixture case file {file_name:?}");
        };
        let cassette: HidCassette = read_json(&entry.path(), "HID cassette")?;
        if cassette.name != *case_name {
            bail!(
                "relationship verification failed: cassette file {file_name:?} contains case {:?}",
                cassette.name
            );
        }
        found.insert(file_name, cassette);
    }

    if let Some(missing) = expected.keys().find(|name| !found.contains_key(*name)) {
        bail!("relationship verification failed: missing declared fixture case file {missing:?}");
    }
    Ok(found.into_values().collect())
}

fn case_file_name(case_name: &str) -> Result<String> {
    if matches!(case_name, "." | "..") || case_name.contains('/') || case_name.contains('\\') {
        bail!(
            "relationship verification failed: fixture case name {case_name:?} is not a safe file name"
        );
    }
    Ok(format!("{case_name}.json"))
}

fn require_directory_id(directory: &Path, fixture_id: &str) -> Result<()> {
    if directory.file_name().and_then(|name| name.to_str()) == Some(fixture_id) {
        Ok(())
    } else {
        bail!(
            "relationship verification failed: fixture directory name must equal manifest id {fixture_id:?}"
        )
    }
}

fn read_json<T: DeserializeOwned>(path: &Path, asset: &str) -> Result<T> {
    let bytes = fs::read(path).with_context(|| {
        format!(
            "fixture structure verification failed: could not read {asset} {}",
            path.display()
        )
    })?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("schema verification failed while parsing {asset}"))
}

fn read_directory(directory: &Path, asset: &str) -> Result<Vec<DirEntry>> {
    let mut entries = fs::read_dir(directory)
        .with_context(|| {
            format!(
                "fixture structure verification failed: could not read {asset} {}",
                directory.display()
            )
        })?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("fixture structure verification failed while reading {asset}"))?;
    entries.sort_by_key(DirEntry::file_name);
    Ok(entries)
}

fn require_directory(path: &Path, asset: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path).with_context(|| {
        format!(
            "fixture structure verification failed: could not inspect {asset} {}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("fixture structure verification failed: {asset} must be a non-symlink directory");
    }
    Ok(())
}

fn require_entry_directory(entry: &DirEntry, asset: &str) -> Result<()> {
    let metadata = entry_metadata(entry, asset)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("fixture structure verification failed: {asset} must be a non-symlink directory");
    }
    Ok(())
}

fn require_regular_file(entry: &DirEntry, asset: &str) -> Result<()> {
    let metadata = entry_metadata(entry, asset)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("fixture structure verification failed: {asset} must be a non-symlink regular file");
    }
    Ok(())
}

fn entry_metadata(entry: &DirEntry, asset: &str) -> Result<std::fs::Metadata> {
    fs::symlink_metadata(entry.path()).with_context(|| {
        format!("fixture structure verification failed: could not inspect {asset}")
    })
}

fn entry_name(entry: &DirEntry) -> Result<String> {
    entry.file_name().into_string().map_err(|_| {
        anyhow::anyhow!("fixture structure verification failed: fixture paths must be valid UTF-8")
    })
}

#[cfg(test)]
mod tests;
