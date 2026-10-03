//! Content-addressed packages. Activation rechecks bytes instead of trusting paths.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
};

use cap_std::{ambient_authority, fs::Dir};
use sha2::{Digest as _, Sha256};

use crate::{
    PluginError,
    descriptor::Descriptor,
    limits,
    manifest::{Manifest, validate_relative_path},
    validate_component,
};

/// One fully read, validated package. These are the bytes compiled and installed.
pub struct Package {
    manifest: Manifest,
    descriptors: Vec<Descriptor>,
    files: BTreeMap<String, Vec<u8>>,
    digest: String,
}

impl Package {
    /// Read a directory through a capability root and reject changes during staging.
    pub fn read(path: &Path) -> Result<Self, PluginError> {
        if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(PluginError::Invalid("package root is a symlink".into()));
        }
        let root = Dir::open_ambient_dir(path, ambient_authority())?;
        let source = read_regular(&root, "plugin.toml", limits::DESCRIPTOR_BYTES)?;
        let manifest = Manifest::parse(utf8(&source)?)?;
        let mut files = BTreeMap::from([("plugin.toml".into(), source)]);
        let mut descriptors = Vec::new();
        let mut ids = BTreeSet::new();
        for name in &manifest.descriptors {
            let bytes = read_regular(&root, name, limits::DESCRIPTOR_BYTES)?;
            let descriptor = Descriptor::parse(utf8(&bytes)?)?;
            manifest.validate_descriptor(&descriptor)?;
            if !ids.insert(descriptor.id().clone()) {
                return Err(PluginError::Invalid(
                    "duplicate descriptor identity in package".into(),
                ));
            }
            descriptors.push(descriptor);
            files.insert(name.clone(), bytes);
        }
        let component = read_regular(&root, &manifest.entry, limits::COMPONENT_BYTES)?;
        validate_component(&component)?;
        files.insert(manifest.entry.clone(), component);
        if files.values().map(Vec::len).sum::<usize>() > limits::PACKAGE_BYTES {
            return Err(PluginError::Invalid("package exceeds 16 MiB".into()));
        }
        let mut actual = BTreeSet::new();
        collect_files(&root, Path::new(""), &mut actual, &mut 0)?;
        if actual != files.keys().cloned().collect() {
            return Err(PluginError::Invalid(
                "package contains unreferenced files".into(),
            ));
        }
        for (name, bytes) in &files {
            if read_regular(&root, name, bytes.len())? != *bytes {
                return Err(PluginError::Invalid(format!(
                    "package changed while staging: {name}"
                )));
            }
        }
        let mut hash = Sha256::new();
        for (name, bytes) in &files {
            hash.update((name.len() as u64).to_le_bytes());
            hash.update(name.as_bytes());
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        let digest = format!("{:x}", hash.finalize());
        Ok(Self {
            manifest,
            descriptors,
            files,
            digest,
        })
    }

    /// Validated manifest.
    #[must_use]
    pub const fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    /// Validated descriptors.
    #[must_use]
    pub fn descriptors(&self) -> &[Descriptor] {
        &self.descriptors
    }
    /// Exact content identity; not publisher authentication.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }
    /// The exact bytes which were hashed during admission.
    #[must_use]
    pub fn component(&self) -> &[u8] {
        &self.files[&self.manifest.entry]
    }

    /// Atomically install already validated bytes without enabling device access.
    pub fn install(&self, store: &Path) -> Result<PathBuf, PluginError> {
        std::fs::create_dir_all(store)?;
        let destination = store.join(&self.digest);
        if destination.try_exists()? {
            let existing = Self::read(&destination)?;
            if existing.digest != self.digest {
                return Err(PluginError::Invalid(
                    "installed content digest changed".into(),
                ));
            }
            return Ok(destination);
        }
        let staging = tempfile::Builder::new()
            .prefix(".staging-")
            .tempdir_in(store)?;
        for (name, bytes) in &self.files {
            let target = staging.path().join(name);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            std::io::Write::write_all(&mut file, bytes)?;
            file.sync_all()?;
            let mut permissions = file.metadata()?.permissions();
            permissions.set_readonly(true);
            file.set_permissions(permissions)?;
        }
        std::fs::rename(staging.path(), &destination)?;
        Ok(destination)
    }

    /// Load only the selected immutable digest, never a substitute version.
    pub fn installed(store: &Path, digest: &str) -> Result<Self, PluginError> {
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(PluginError::Invalid("invalid package digest".into()));
        }
        let package = Self::read(&store.join(digest))?;
        if package.digest != digest {
            return Err(PluginError::Invalid(
                "installed content does not match the selected digest".into(),
            ));
        }
        Ok(package)
    }

    /// Remove only verified content from the private package store.
    pub fn remove(&self, store: &Path) -> Result<(), PluginError> {
        Self::installed(store, &self.digest)?;
        let directory = store.join(&self.digest);
        #[cfg(windows)]
        for name in self.files.keys() {
            let path = directory.join(name);
            let mut permissions = std::fs::metadata(&path)?.permissions();
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "Windows must clear FILE_ATTRIBUTE_READONLY before removal; this branch cannot change Unix mode bits"
            )]
            permissions.set_readonly(false);
            std::fs::set_permissions(path, permissions)?;
        }
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}

fn utf8(bytes: &[u8]) -> Result<&str, PluginError> {
    std::str::from_utf8(bytes)
        .map_err(|e| PluginError::Invalid(format!("manifest/descriptor is not UTF-8: {e}")))
}

fn read_regular(root: &Dir, name: &str, limit: usize) -> Result<Vec<u8>, PluginError> {
    validate_relative_path(name)?;
    let mut prefix = PathBuf::new();
    for part in Path::new(name) {
        prefix.push(part);
        if root.symlink_metadata(&prefix)?.file_type().is_symlink() {
            return Err(PluginError::Invalid(format!("package symlink: {name}")));
        }
    }
    let file = root.open(name)?;
    if !file.metadata()?.is_file() {
        return Err(PluginError::Invalid(format!(
            "not a regular package file: {name}"
        )));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(PluginError::Invalid(format!(
            "package file exceeds its limit: {name}"
        )));
    }
    Ok(bytes)
}

fn collect_files(
    root: &Dir,
    parent: &Path,
    names: &mut BTreeSet<String>,
    entries: &mut usize,
) -> Result<(), PluginError> {
    if parent.components().count() > 16 || names.len() > limits::SELECTORS + 2 {
        return Err(PluginError::Invalid(
            "package directory exceeds limits".into(),
        ));
    }
    for entry in root.entries()? {
        *entries += 1;
        if *entries > 256 {
            return Err(PluginError::Invalid(
                "package contains more than 256 directory entries".into(),
            ));
        }
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| PluginError::Invalid("package filename is not UTF-8".into()))?;
        let path = parent.join(&name);
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(PluginError::Invalid("package contains a symlink".into()));
        }
        if kind.is_dir() {
            collect_files(&root.open_dir(name)?, &path, names, entries)?;
        } else if kind.is_file() {
            names.insert(path.to_string_lossy().replace('\\', "/"));
        } else {
            return Err(PluginError::Invalid(
                "package contains a non-regular file".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_admission_confines_files_and_removal_requires_the_original_content() {
        let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/plugins/counter-button/package");
        let original = Package::read(&sample).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        for (name, bytes) in &original.files {
            std::fs::write(source.join(name), bytes).unwrap();
        }
        let manifest = std::fs::read_to_string(source.join("plugin.toml")).unwrap();
        for entry in [
            "../driver.wasm",
            "/driver.wasm",
            "C:/driver.wasm",
            "sub/../driver.wasm",
        ] {
            std::fs::write(
                source.join("plugin.toml"),
                manifest.replace("entry = \"driver.wasm\"", &format!("entry = {entry:?}")),
            )
            .unwrap();
            assert!(
                matches!(Package::read(&source), Err(PluginError::Invalid(message)) if message.contains("traversal"))
            );
        }
        std::fs::write(source.join("plugin.toml"), &manifest).unwrap();
        std::fs::write(source.join("unreferenced"), "extra").unwrap();
        assert!(
            matches!(Package::read(&source), Err(PluginError::Invalid(message)) if message.contains("unreferenced"))
        );
        std::fs::remove_file(source.join("unreferenced")).unwrap();
        #[cfg(unix)]
        {
            let descriptor = source.join("counter.device.toml");
            std::fs::remove_file(&descriptor).unwrap();
            std::os::unix::fs::symlink(sample.join("counter.device.toml"), &descriptor).unwrap();
            assert!(
                matches!(Package::read(&source), Err(PluginError::Invalid(message)) if message.contains("symlink"))
            );
            std::fs::remove_file(&descriptor).unwrap();
            std::fs::copy(sample.join("counter.device.toml"), &descriptor).unwrap();
        }
        let store = temp.path().join("store");
        let package = Package::read(&source).unwrap();
        let installed = package.install(&store).unwrap();
        assert_eq!(
            Package::installed(&store, package.digest())
                .unwrap()
                .component(),
            original.component()
        );
        assert_eq!(package.install(&store).unwrap(), installed);
        assert!(matches!(
            Package::installed(&store, "../source"),
            Err(PluginError::Invalid(_))
        ));

        let intact = temp.path().join("intact");
        std::fs::rename(&installed, &intact).unwrap();
        std::fs::create_dir(&installed).unwrap();
        for (name, bytes) in &package.files {
            std::fs::write(installed.join(name), bytes).unwrap();
        }
        std::fs::write(
            installed.join("plugin.toml"),
            manifest.replace("0.1.0", "0.1.1"),
        )
        .unwrap();
        assert!(
            matches!(Package::installed(&store, package.digest()), Err(PluginError::Invalid(message)) if message.contains("digest"))
        );
        assert!(matches!(
            package.remove(&store),
            Err(PluginError::Invalid(_))
        ));
        assert!(
            installed.exists(),
            "removal must preserve unverified content"
        );
        std::fs::remove_dir_all(&installed).unwrap();
        std::fs::rename(intact, &installed).unwrap();
        package.remove(&store).unwrap();
        assert!(!installed.exists());
        assert!(
            source.exists(),
            "removal must stay within the package store"
        );
    }
}
