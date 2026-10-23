//! `release stamp-version`: give the workspace a release's own version before
//! it builds, without committing.
//!
//! A beta tags `master` without a version bump, so its tree carries the
//! version of the stable release it follows. Its build must carry the beta's
//! version instead: the built-in update engine compares the running version
//! with the channel's, and every `CARGO_PKG_VERSION` (the About page,
//! diagnostics, the Windows version resource, the Linux packages, the
//! AppStream release) shows it. This rewrites what release-plz's bump would:
//! `[workspace.package].version`, the `version` of every path dependency and
//! `[patch]` entry that points at a workspace crate, and the workspace
//! crates' entries in `Cargo.lock`. A dependency is a workspace crate's by
//! its `path`, not its name, so a registry or git crate of the same name is
//! left alone. Nothing else in the tree changes, and the stamped tree must
//! resolve as a `--locked` build would before the command succeeds.

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

/// Stamp `--version` into the workspace and check the result.
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
    match stamp_checked(&sh, &root, &members, &args.version)? {
        Stamped::Unchanged => println!("the workspace is already at {}", args.version),
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

/// [`stamp`], then [`check`] the tree. If either fails, it tries to put every
/// file the stamp may have written back to its bytes from before, and names
/// each file it could not restore. Restoring is a second round of writes, so
/// a disk that failed the stamp can fail it too.
pub(crate) fn stamp_checked(
    sh: &Shell,
    root: &Path,
    members: &[Member],
    version: &Version,
) -> Result<Stamped> {
    let mut files: BTreeSet<PathBuf> = BTreeSet::new();
    for path in members
        .iter()
        .map(|member| member.manifest.clone())
        .chain([root.join("Cargo.toml"), root.join("Cargo.lock")])
    {
        files.insert(fs_err::canonicalize(path)?);
    }
    let before = files
        .into_iter()
        .map(|path| Ok((fs_err::read(&path)?, path)))
        .collect::<Result<Vec<_>>>()?;
    let stamped = stamp(root, members, version).and_then(|stamped| {
        check(sh, root, members, version)?;
        Ok(stamped)
    });
    let Err(error) = stamped else {
        return stamped;
    };
    let failed = restore(&before);
    if failed.is_empty() {
        return Err(error.context(format!("stamping {version} failed; the tree is as it was")));
    }
    Err(error.context(format!(
        "stamping {version} failed, and {} of its files could not be restored: {}",
        failed.len(),
        failed.join("; ")
    )))
}

/// Write each file back to its bytes, unless it already has them. A file that
/// cannot be written does not stop the others; the errors come back, one per
/// file.
fn restore(files: &[(Vec<u8>, PathBuf)]) -> Vec<String> {
    files
        .iter()
        .filter_map(|(bytes, path)| {
            let restored = match fs_err::read(path) {
                Ok(now) if &now == bytes => Ok(()),
                _ => fs_err::write(path, bytes),
            };
            restored.err().map(|error| error.to_string())
        })
        .collect()
}

/// Resolve the tree under `root` as a `--locked` build would, and require
/// every one of `members` to report `version`.
///
/// `cargo metadata` without `--no-deps` resolves every dependency against
/// `Cargo.lock` and fails if the lock would have to change. So it catches a
/// lock entry or `[patch]` the stamp left behind, and a requirement the new
/// version does not meet. A requirement the new version still meets, such as
/// `0.8.18-beta.1` for `0.8.18-beta.2`, resolves; [`stamp`] rewrites those
/// itself, editing each manifest once.
fn check(sh: &Shell, root: &Path, members: &[Member], version: &Version) -> Result<()> {
    let _root = sh.push_dir(root);
    let json = cmd!(sh, "cargo metadata --locked --format-version 1")
        .quiet()
        .read()
        .context("the stamped tree does not resolve with `--locked`")?;
    let metadata: Metadata =
        serde_json::from_str(&json).context("parsing `cargo metadata` output")?;
    let mut unchecked: BTreeMap<PathBuf, &str> = BTreeMap::new();
    for member in members {
        unchecked.insert(fs_err::canonicalize(&member.manifest)?, &member.name);
    }
    let expected = version.to_string();
    for package in &metadata.packages {
        let Ok(manifest) = fs_err::canonicalize(&package.manifest_path) else {
            continue;
        };
        if unchecked.remove(&manifest).is_some() {
            ensure!(
                package.version == expected,
                "{} reports {} after stamping {expected}",
                package.name,
                package.version
            );
        }
    }
    ensure!(
        unchecked.is_empty(),
        "`cargo metadata` does not list {:?}",
        unchecked.into_values().collect::<Vec<_>>()
    );
    Ok(())
}

/// Stamp `version` into the workspace under `root`, whose crates are
/// `members`. Every edit is made in memory first, so a manifest that does not
/// look as expected fails the stamp before any file is written. A manifest is
/// one file however its path is spelled: the root's own manifest can also be
/// a member's, so edits are kept by canonical path and each file is written
/// once. The writes themselves are one file at a time; [`stamp_checked`] tries
/// to undo them if one fails.
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
    let mut dirs = BTreeMap::new();
    for member in members {
        let dir = member
            .manifest
            .parent()
            .with_context(|| format!("{} has no directory", member.manifest.display()))?;
        dirs.insert(fs_err::canonicalize(dir)?, member.name.as_str());
    }
    let rename = Rename {
        names: &names,
        dirs: &dirs,
        from: &from_text,
        to: &to_text,
    };

    root_doc["workspace"]["package"]["version"] = toml_edit::value(to_text.as_str());
    let mut requirements = 0;
    if let Some(table) = root_doc["workspace"]
        .get_mut("dependencies")
        .and_then(Item::as_table_like_mut)
    {
        requirements += rename.requirements(table, root).with_context(|| {
            format!("in [workspace.dependencies] of {}", root_manifest.display())
        })?;
    }
    if let Some(patches) = root_doc.get_mut("patch").and_then(Item::as_table_like_mut) {
        for (source, table) in patches.iter_mut() {
            if let Some(table) = table.as_table_like_mut() {
                requirements += rename.requirements(table, root).with_context(|| {
                    format!("in [patch.{}] of {}", source.get(), root_manifest.display())
                })?;
            }
        }
    }

    let mut docs: BTreeMap<PathBuf, DocumentMut> = BTreeMap::new();
    docs.insert(fs_err::canonicalize(&root_manifest)?, root_doc);
    for member in members {
        let path = &member.manifest;
        let doc = match docs.entry(fs_err::canonicalize(path)?) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(read(path)?),
        };
        let dir = path.parent().unwrap_or(root);
        requirements += rename
            .member(doc, dir)
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

/// One stamp: the workspace crates' names and directories, and the version
/// moving `from` → `to`, both as written in the manifests.
struct Rename<'a> {
    names: &'a BTreeSet<&'a str>,
    /// Each workspace crate's directory, canonical, and its package name.
    dirs: &'a BTreeMap<PathBuf, &'a str>,
    from: &'a str,
    to: &'a str,
}

/// The dependency tables of a manifest, outside `[target.*]`.
const DEPENDENCY_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

impl Rename<'_> {
    /// A crate's own version, if it does not inherit the workspace's, and
    /// its requirements on workspace crates, in every dependency table
    /// including the `[target.*]` ones. `dir` is the crate's directory, which
    /// its dependency paths are relative to.
    fn member(&self, doc: &mut DocumentMut, dir: &Path) -> Result<usize> {
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
                    .requirements(table, dir)
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
                            .requirements(table, dir)
                            .with_context(|| format!("in [target.{target}.{name}]"))?;
                    }
                }
            }
        }
        Ok(requirements)
    }

    /// The `version` requirement of each entry in `table` whose `path`,
    /// relative to `dir`, is a workspace crate's directory. The path decides,
    /// not the name: a bare `"1.0"` or an entry without `path` is a registry
    /// or git crate even when a workspace crate has its name. An entry that
    /// inherits from `[workspace.dependencies]` or names no version has
    /// nothing to stamp.
    fn requirements(&self, table: &mut dyn TableLike, dir: &Path) -> Result<usize> {
        let mut requirements = 0;
        for (key, item) in table.iter_mut() {
            let Some(entry) = item.as_table_like_mut() else {
                continue;
            };
            let Some(path) = entry.get("path").and_then(Item::as_str) else {
                continue;
            };
            let Some(package) = fs_err::canonicalize(dir.join(path))
                .ok()
                .and_then(|path| self.dirs.get(&path))
            else {
                continue;
            };
            if let Some(version) = entry.get_mut("version") {
                let label = format!("the requirement on {package} ({})", key.get());
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
