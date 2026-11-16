//! Finding the installed modules: one folder each under the modules
//! directory, described by its `module.toml`.

use std::collections::BTreeMap;
use std::fs;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::io;
use std::path::{Path, PathBuf};

use openlogi_core::module::{MANIFEST_FILE, ModuleId, ModuleManifest};

/// The largest `module.toml` read.
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

/// One folder in the modules directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Found {
    /// The folder's name.
    pub(super) folder: String,
    /// The folder.
    pub(super) path: PathBuf,
    /// Moves whenever the manifest or the executable changes, so a module
    /// updated in place restarts and a failed one gets another chance.
    pub(super) fingerprint: u64,
    /// The manifest, or why the folder holds no module that can run.
    pub(super) manifest: Result<ModuleManifest, String>,
}

/// Every folder under `dir`, in folder-name order. A folder whose name
/// starts with `.` is not a module. A missing `dir` has no modules; an
/// unreadable one is an error, so the caller can keep what it last found.
///
/// A folder whose manifest names an id an earlier folder already has is
/// refused, so one id always means one module.
pub(super) fn scan(dir: &Path) -> io::Result<Vec<Found>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut folders: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let folder = entry.file_name().into_string().ok()?;
            let path = entry.path();
            (!folder.starts_with('.') && path.is_dir()).then_some((folder, path))
        })
        .collect();
    folders.sort();
    let mut owners: BTreeMap<ModuleId, String> = BTreeMap::new();
    Ok(folders
        .into_iter()
        .map(|(folder, path)| {
            let (fingerprint, mut manifest) = read_folder(&path);
            if let Ok(found) = &manifest {
                if let Some(owner) = owners.get(found.id()) {
                    manifest = Err(format!(
                        "the module in folder {owner:?} already has the id {}",
                        found.id()
                    ));
                } else {
                    owners.insert(found.id().clone(), folder.clone());
                }
            }
            Found {
                folder,
                path,
                fingerprint,
                manifest,
            }
        })
        .collect())
}

/// The fingerprint and manifest of the module folder `path`.
fn read_folder(path: &Path) -> (u64, Result<ModuleManifest, String>) {
    let mut fingerprint = DefaultHasher::new();
    let manifest = read_manifest(&path.join(MANIFEST_FILE)).and_then(|source| {
        source.hash(&mut fingerprint);
        let manifest = ModuleManifest::parse(&source).map_err(|error| error.to_string())?;
        let executable = manifest.executable_in(path);
        let metadata = fs::metadata(&executable)
            .ok()
            .filter(fs::Metadata::is_file)
            .ok_or_else(|| format!("the executable {} is not there", manifest.executable()))?;
        metadata.len().hash(&mut fingerprint);
        metadata.modified().ok().hash(&mut fingerprint);
        Ok(manifest)
    });
    (fingerprint.finish(), manifest)
}

fn read_manifest(path: &Path) -> Result<String, String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(format!("no {MANIFEST_FILE}"));
        }
        Err(error) => return Err(format!("{MANIFEST_FILE} cannot be read: {error}")),
    };
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "{MANIFEST_FILE} is larger than {} KiB",
            MAX_MANIFEST_BYTES / 1024
        ));
    }
    fs::read_to_string(path).map_err(|error| format!("{MANIFEST_FILE} cannot be read: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(id: &str, executable: &str) -> String {
        format!(
            "id = \"{id}\"\nname = \"Test\"\nprotocol = 1\nplatforms = [\"macos\", \"linux\", \"windows\"]\nexecutable = \"{executable}\"\n"
        )
    }

    fn module(dir: &Path, folder: &str, manifest: Option<&str>, executable: Option<&str>) {
        let path = dir.join(folder);
        fs::create_dir_all(&path).unwrap();
        if let Some(manifest) = manifest {
            fs::write(path.join(MANIFEST_FILE), manifest).unwrap();
        }
        if let Some(executable) = executable {
            fs::write(path.join(executable), "#!/bin/sh\n").unwrap();
        }
    }

    #[test]
    fn a_missing_modules_folder_holds_no_modules() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(scan(&dir.path().join("modules")).unwrap(), []);
    }

    #[test]
    fn every_folder_is_listed_with_its_manifest_or_its_error() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        module(
            dir,
            "b-good",
            Some(&manifest("com.example.good", "run")),
            Some("run"),
        );
        module(dir, "a-empty", None, None);
        module(dir, "c-broken", Some("id = "), None);
        module(
            dir,
            "d-noexe",
            Some(&manifest("com.example.noexe", "run")),
            None,
        );
        module(
            dir,
            "e-twin",
            Some(&manifest("com.example.good", "run")),
            Some("run"),
        );
        module(
            dir,
            ".hidden",
            Some(&manifest("com.example.hidden", "run")),
            Some("run"),
        );
        fs::write(dir.join("stray-file"), "not a module").unwrap();
        module(
            dir,
            "f-huge",
            Some(&"#".repeat(usize::try_from(MAX_MANIFEST_BYTES).unwrap() + 1)),
            None,
        );

        let found = scan(dir).unwrap();
        let folders: Vec<_> = found.iter().map(|found| found.folder.as_str()).collect();
        assert_eq!(
            folders,
            [
                "a-empty", "b-good", "c-broken", "d-noexe", "e-twin", "f-huge"
            ]
        );
        let error = |index: usize| found[index].manifest.clone().unwrap_err();
        assert_eq!(error(0), "no module.toml");
        assert_eq!(
            found[1].manifest.as_ref().unwrap().id().as_ref(),
            "com.example.good"
        );
        assert!(error(2).contains("invalid"), "{}", error(2));
        assert!(error(3).contains("not there"), "{}", error(3));
        assert!(error(4).contains("already has the id"), "{}", error(4));
        assert!(error(5).contains("larger than"), "{}", error(5));
    }

    #[test]
    fn the_fingerprint_moves_when_the_manifest_or_the_executable_changes() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        module(
            dir,
            "m",
            Some(&manifest("com.example.m", "run")),
            Some("run"),
        );
        let first = scan(dir).unwrap()[0].fingerprint;
        assert_eq!(
            scan(dir).unwrap()[0].fingerprint,
            first,
            "stable when nothing changed"
        );

        fs::write(dir.join("m/run"), "#!/bin/sh\necho longer\n").unwrap();
        let second = scan(dir).unwrap()[0].fingerprint;
        assert_ne!(second, first, "a new executable");

        fs::write(
            dir.join("m").join(MANIFEST_FILE),
            manifest("com.example.m", "run") + "version = \"2\"\n",
        )
        .unwrap();
        assert_ne!(scan(dir).unwrap()[0].fingerprint, second, "a new manifest");
    }
}
