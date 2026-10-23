//! `release stamp-version`: give the workspace a release's own version before
//! it builds, without committing.
//!
//! A beta tags `master` without a version bump, so its tree carries the
//! version of the stable release it follows. Its build must carry the beta's
//! version instead: the built-in update engine compares the running version
//! with the channel's, and every `CARGO_PKG_VERSION` (the About page,
//! diagnostics, the Windows version resource, the Linux packages, the
//! AppStream release) shows it. This rewrites what release-plz's bump would:
//! `[workspace.package].version`, every requirement on a workspace crate,
//! and the workspace crates' entries in `Cargo.lock`, so a `--locked` build
//! still resolves. Nothing else in the tree changes.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use clap::Parser;
use semver::Version;
use serde::Deserialize;
use toml_edit::{DocumentMut, Item, TableLike, Value};
use xshell::{Shell, cmd};

use crate::support::fs::repo_root;

#[derive(Parser)]
pub(crate) struct Args {
    /// The version to stamp, as `release version` prints it in
    /// `stamp_version`, for example `0.8.18-beta.3`.
    #[arg(long)]
    version: Version,
}

/// A workspace crate: its package name and its manifest.
#[derive(Debug, Clone)]
pub(crate) struct Member {
    pub(crate) name: String,
    pub(crate) manifest: PathBuf,
}

/// What [`stamp`] changed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Stamped {
    /// The workspace already carried the version.
    Unchanged,
    /// The workspace moved from `from`; `requirements` requirements on
    /// workspace crates and `locked` `Cargo.lock` entries followed.
    Changed {
        from: Version,
        requirements: usize,
        locked: usize,
    },
}

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<MetadataPackage>,
}

#[derive(Deserialize)]
struct MetadataPackage {
    name: String,
    version: String,
    manifest_path: PathBuf,
}

/// Stamp `--version` into the workspace, then check with `cargo metadata`
/// that every workspace crate reports it.
pub(crate) fn run(args: &Args) -> Result<()> {
    let root = repo_root()?;
    let sh = Shell::new()?;
    sh.change_dir(&root);
    let members = workspace_crates(&sh)?
        .into_iter()
        .map(|package| Member {
            name: package.name,
            manifest: package.manifest_path,
        })
        .collect::<Vec<_>>();
    match stamp(&root, &members, &args.version)? {
        Stamped::Unchanged => {
            println!("the workspace is already at {}", args.version);
            return Ok(());
        }
        Stamped::Changed {
            from,
            requirements,
            locked,
        } => println!(
            "stamped {} over {from}: {requirements} requirements on workspace crates, \
             {locked} Cargo.lock entries",
            args.version
        ),
    }
    let expected = args.version.to_string();
    for package in workspace_crates(&sh)? {
        ensure!(
            package.version == expected,
            "{} reports {} after stamping {expected}",
            package.name,
            package.version
        );
    }
    Ok(())
}

/// The workspace crates, as `cargo metadata --no-deps` lists them. It reads
/// only the manifests, so it neither resolves nor touches `Cargo.lock`.
fn workspace_crates(sh: &Shell) -> Result<Vec<MetadataPackage>> {
    let json = cmd!(sh, "cargo metadata --no-deps --format-version 1")
        .quiet()
        .read()?;
    let metadata: Metadata =
        serde_json::from_str(&json).context("parsing `cargo metadata` output")?;
    Ok(metadata.packages)
}

/// Stamp `version` into the workspace under `root`, whose crates are
/// `members`. Every edit is made in memory first, so a manifest that does not
/// look as expected fails the stamp before any file is written.
pub(crate) fn stamp(root: &Path, members: &[Member], version: &Version) -> Result<Stamped> {
    let root_manifest = root.join("Cargo.toml");
    let mut root_doc = read(&root_manifest)?;
    let from = workspace_version(&root_doc).with_context(|| {
        format!(
            "reading the workspace version in {}",
            root_manifest.display()
        )
    })?;
    if &from == version {
        return Ok(Stamped::Unchanged);
    }
    let (from_text, to_text) = (from.to_string(), version.to_string());
    let names: BTreeSet<&str> = members.iter().map(|member| member.name.as_str()).collect();
    let rename = Rename {
        names: &names,
        from: &from_text,
        to: &to_text,
    };

    root_doc["workspace"]["package"]["version"] = toml_edit::value(to_text.as_str());
    let mut requirements = 0;
    if let Some(table) = root_doc["workspace"]
        .get_mut("dependencies")
        .and_then(Item::as_table_like_mut)
    {
        requirements += rename.requirements(table).with_context(|| {
            format!("in [workspace.dependencies] of {}", root_manifest.display())
        })?;
    }

    let mut docs: BTreeMap<PathBuf, DocumentMut> = BTreeMap::new();
    docs.insert(root_manifest.clone(), root_doc);
    for member in members {
        let path = &member.manifest;
        let doc = match docs.entry(path.clone()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(read(path)?),
        };
        requirements += rename
            .member(doc)
            .with_context(|| format!("in {}", path.display()))?;
    }

    let lock_path = root.join("Cargo.lock");
    let mut lock = read(&lock_path)?;
    let locked = rename
        .lock(&mut lock)
        .with_context(|| format!("in {}", lock_path.display()))?;
    ensure!(
        locked == names.len(),
        "{} lists {locked} of the {} workspace crates",
        lock_path.display(),
        names.len()
    );

    for (path, doc) in &docs {
        fs_err::write(path, doc.to_string())?;
    }
    fs_err::write(&lock_path, lock.to_string())?;
    Ok(Stamped::Changed {
        from,
        requirements,
        locked,
    })
}

fn read(path: &Path) -> Result<DocumentMut> {
    fs_err::read_to_string(path)?
        .parse()
        .with_context(|| format!("parsing {}", path.display()))
}

fn workspace_version(doc: &DocumentMut) -> Result<Version> {
    let text = doc
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("version"))
        .and_then(Item::as_str)
        .context("no [workspace.package] version")?;
    Version::parse(text).with_context(|| format!("{text:?} is not a semver version"))
}

/// One stamp: the workspace crates' names and the version moving `from` →
/// `to`, both as written in the manifests.
struct Rename<'a> {
    names: &'a BTreeSet<&'a str>,
    from: &'a str,
    to: &'a str,
}

/// The dependency tables of a manifest, outside `[target.*]`.
const DEPENDENCY_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

impl Rename<'_> {
    /// A crate's own version, if it does not inherit the workspace's, and
    /// its requirements on workspace crates, in every dependency table
    /// including the `[target.*]` ones.
    fn member(&self, doc: &mut DocumentMut) -> Result<usize> {
        if let Some(own) = doc
            .get_mut("package")
            .and_then(|package| package.get_mut("version"))
            .filter(|own| own.is_str())
        {
            self.version(own, "[package] version")?;
        }
        let mut requirements = 0;
        for name in DEPENDENCY_TABLES {
            if let Some(table) = doc.get_mut(name).and_then(Item::as_table_like_mut) {
                requirements += self
                    .requirements(table)
                    .with_context(|| format!("in [{name}]"))?;
            }
        }
        if let Some(targets) = doc.get_mut("target").and_then(Item::as_table_like_mut) {
            for (target, item) in targets.iter_mut() {
                let Some(target_table) = item.as_table_like_mut() else {
                    continue;
                };
                for name in DEPENDENCY_TABLES {
                    if let Some(table) =
                        target_table.get_mut(name).and_then(Item::as_table_like_mut)
                    {
                        requirements += self
                            .requirements(table)
                            .with_context(|| format!("in [target.{target}.{name}]"))?;
                    }
                }
            }
        }
        Ok(requirements)
    }

    /// The `version` requirement of each dependency on a workspace crate in
    /// `table`. A crate is matched by its package name, so a renamed
    /// dependency (`package = "…"`) counts. One that inherits from
    /// `[workspace.dependencies]` or names no version has nothing to stamp.
    fn requirements(&self, table: &mut dyn TableLike) -> Result<usize> {
        let mut requirements = 0;
        for (key, item) in table.iter_mut() {
            let package = item
                .get("package")
                .and_then(Item::as_str)
                .unwrap_or_else(|| key.get())
                .to_owned();
            if !self.names.contains(package.as_str()) {
                continue;
            }
            let label = format!("the requirement on {package}");
            if item.is_str() {
                self.version(item, &label)?;
                requirements += 1;
            } else if let Some(version) = item
                .as_table_like_mut()
                .and_then(|dep| dep.get_mut("version"))
            {
                self.version(version, &label)?;
                requirements += 1;
            }
        }
        Ok(requirements)
    }

    /// The workspace crates' `[[package]]` entries in `Cargo.lock`, and the
    /// `"name version"` references a lock file uses when a name is ambiguous.
    /// Returns how many entries moved.
    fn lock(&self, lock: &mut DocumentMut) -> Result<usize> {
        let packages = lock
            .get_mut("package")
            .and_then(Item::as_array_of_tables_mut)
            .context("no [[package]] entries")?;
        let mut locked = 0;
        for package in packages.iter_mut() {
            let name = package
                .get("name")
                .and_then(Item::as_str)
                .unwrap_or_default()
                .to_owned();
            // A registry or git package of the same name has a `source`; only
            // the workspace's own path crates have none.
            if self.names.contains(name.as_str()) && !package.contains_key("source") {
                let version = package
                    .get_mut("version")
                    .with_context(|| format!("{name} has no version"))?;
                self.version(version, &format!("the entry for {name}"))?;
                locked += 1;
            }
            if let Some(dependencies) = package.get_mut("dependencies").and_then(Item::as_array_mut)
            {
                for dependency in dependencies.iter_mut() {
                    let Some((name, version)) =
                        dependency.as_str().and_then(|text| text.split_once(' '))
                    else {
                        continue;
                    };
                    if self.names.contains(name) && version == self.from {
                        let to = format!("{name} {}", self.to);
                        replace(dependency, &to);
                    }
                }
            }
        }
        Ok(locked)
    }

    /// Move one version string from `from` to `to`; anything else is a
    /// requirement release-plz would not have written, so stamping it would
    /// be a guess.
    fn version(&self, item: &mut Item, label: &str) -> Result<()> {
        let Some(value) = item.as_value_mut() else {
            bail!("{label} is not a string");
        };
        match value.as_str() {
            Some(text) if text == self.from => {
                replace(value, self.to);
                Ok(())
            }
            Some(text) => bail!(
                "{label} is {text:?}, not the workspace version {:?}",
                self.from
            ),
            None => bail!("{label} is not a string"),
        }
    }
}

/// Replace a string value, keeping the whitespace and comments around it.
fn replace(value: &mut Value, text: &str) {
    let decor = value.decor().clone();
    *value = Value::from(text);
    *value.decor_mut() = decor;
}

#[cfg(test)]
mod tests;
