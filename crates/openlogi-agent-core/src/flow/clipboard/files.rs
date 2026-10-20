//! Files on the clipboard: what is copied here, walked into the files a peer
//! can fetch, and where the files fetched from a peer go so a paste can copy
//! them.
//!
//! Each transfer gets a folder of its own under [`RECEIVED_DIR`], named for
//! when it started. A transfer that fails or is superseded is removed
//! whole; of the complete ones, the newest [`KEEP`] stay.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use openlogi_flow::clipboard::{
    FetchError, FileListing, ListedFile, ListedFolder, MAX_FILES, MAX_FILES_BYTES, OfferedFile,
    fetch_file, is_safe_relative_path,
};
use openlogi_flow::transport::FlowConnection;
use tracing::debug;

use super::ReceivedFiles;

/// The folder, under OpenLogi's data directory, that holds received files.
pub(super) const RECEIVED_DIR: &str = "flow-received";

/// How many transfers stay: the one on the clipboard, and the one before,
/// which a paste may still be copying from.
const KEEP: usize = 2;

/// The longest a peer may go silent while it sends a file.
const STALL: Duration = Duration::from_secs(10);

/// Why the copied files cannot be offered.
#[derive(Debug, thiserror::Error)]
pub(super) enum CollectError {
    /// More than [`MAX_FILES`] files and empty folders, or [`MAX_FILES_BYTES`]
    /// bytes.
    #[error("the files exceed the sync limit of {MAX_FILES} files and {MAX_FILES_BYTES} bytes")]
    TooLarge,
    /// A name is not Unicode, or another computer could not create it.
    #[error("a copied file's name cannot be sent")]
    Name,
    /// A copied file or folder could not be read.
    #[error("the copied files could not be read: {0}")]
    Io(#[from] io::Error),
}

/// What was copied, as clipboard sync offers it.
#[derive(Clone, Debug, Default)]
pub(super) struct CopiedFiles {
    pub(super) files: Vec<OfferedFile>,
    /// The copied folders that hold no file, `/`-separated: the files' paths
    /// imply the others.
    pub(super) folders: Vec<String>,
}

/// The files under `roots` — the copied files and folders, in clipboard
/// order — as offered files, and the folders among them that hold none.
/// Folders are walked in name order, and links are skipped, so nothing
/// outside what was copied is reached.
pub(super) fn collect(roots: &[PathBuf]) -> Result<CopiedFiles, CollectError> {
    let mut walk = Walk::default();
    let mut taken = HashSet::new();
    for root in roots {
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(CollectError::Name)?;
        let name = unique_name(name, &mut taken);
        let metadata = fs::symlink_metadata(root)?;
        if metadata.is_dir() {
            walk.folder(root, &name)?;
        } else if metadata.is_file() {
            walk.file(root, name, &metadata)?;
        }
    }
    Ok(walk.copied)
}

#[derive(Default)]
struct Walk {
    copied: CopiedFiles,
    bytes: u64,
}

impl Walk {
    /// Whether one more file or empty folder at `relative_path` can go.
    fn admit(&self, relative_path: &str) -> Result<(), CollectError> {
        if self.copied.files.len() + self.copied.folders.len() == MAX_FILES {
            return Err(CollectError::TooLarge);
        }
        if !is_safe_relative_path(relative_path) {
            return Err(CollectError::Name);
        }
        Ok(())
    }

    fn folder(&mut self, folder: &Path, relative: &str) -> Result<(), CollectError> {
        let before = (self.copied.files.len(), self.copied.folders.len());
        let mut entries = fs::read_dir(folder)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| CollectError::Name)?;
            let path = format!("{relative}/{name}");
            if kind.is_dir() {
                self.folder(&entry.path(), &path)?;
            } else if kind.is_file() {
                self.file(&entry.path(), path, &entry.metadata()?)?;
            }
        }
        if (self.copied.files.len(), self.copied.folders.len()) == before {
            // Nothing under it was taken, so no file path implies it.
            self.admit(relative)?;
            self.copied.folders.push(relative.to_owned());
        }
        Ok(())
    }

    fn file(
        &mut self,
        source: &Path,
        relative_path: String,
        metadata: &fs::Metadata,
    ) -> Result<(), CollectError> {
        self.admit(&relative_path)?;
        self.bytes = self
            .bytes
            .checked_add(metadata.len())
            .filter(|bytes| *bytes <= MAX_FILES_BYTES)
            .ok_or(CollectError::TooLarge)?;
        self.copied.files.push(OfferedFile {
            relative_path,
            size_bytes: metadata.len(),
            modified_at_ms: modified_ms(metadata),
            source: source.to_owned(),
        });
        Ok(())
    }
}

/// `name`, or `name (2)`, `name (3)`… when a copied item of the same name
/// came first: items copied from different folders can share one.
fn unique_name(name: &str, taken: &mut HashSet<String>) -> String {
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, Some(extension)),
        _ => (name, None),
    };
    let mut candidate = name.to_owned();
    let mut number = 2;
    while !taken.insert(candidate.to_lowercase()) {
        candidate = match extension {
            Some(extension) => format!("{stem} ({number}).{extension}"),
            None => format!("{stem} ({number})"),
        };
        number += 1;
    }
    candidate
}

fn modified_ms(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .and_then(|since| u64::try_from(since.as_millis()).ok())
        .unwrap_or(0)
}

/// Fetches every file of `listing` into a new transfer folder under `root`,
/// keeping their paths and modification times, and makes its empty folders.
/// `still_wanted` is asked as the files come in; on any failure the folder
/// is removed.
pub(super) async fn receive(
    connection: &FlowConnection,
    sequence: u64,
    listing: &FileListing,
    root: &Path,
    still_wanted: &impl Fn() -> bool,
) -> Result<ReceivedFiles, FetchError> {
    let listed = listing.files();
    let folders: Vec<&str> = listing
        .folders()
        .iter()
        .map(ListedFolder::relative_path)
        .collect();
    let folder = new_folder(root).await.map_err(FetchError::Write)?;
    if let Err(error) = receive_into(
        connection,
        sequence,
        listed,
        &folders,
        &folder,
        still_wanted,
    )
    .await
    {
        discard(&folder);
        return Err(error);
    }
    let mut top_level: Vec<PathBuf> = Vec::new();
    for relative_path in listed
        .iter()
        .map(ListedFile::relative_path)
        .chain(folders.iter().copied())
    {
        let first = relative_path.split('/').next().unwrap_or(relative_path);
        let path = folder.join(first);
        if !top_level.contains(&path) {
            top_level.push(path);
        }
    }
    Ok(ReceivedFiles {
        top_level,
        total_bytes: listed.iter().map(ListedFile::size_bytes).sum(),
        copied: CopiedFiles {
            files: listed
                .iter()
                .map(|file| OfferedFile {
                    relative_path: file.relative_path().to_owned(),
                    size_bytes: file.size_bytes(),
                    modified_at_ms: file.modified_at_ms(),
                    source: local_path(&folder, file.relative_path()),
                })
                .collect(),
            folders: folders.iter().map(|&path| path.to_owned()).collect(),
        },
        folder,
    })
}

async fn receive_into(
    connection: &FlowConnection,
    sequence: u64,
    listed: &[ListedFile],
    folders: &[&str],
    folder: &Path,
    still_wanted: &impl Fn() -> bool,
) -> Result<(), FetchError> {
    for relative_path in folders {
        tokio::fs::create_dir_all(local_path(folder, relative_path))
            .await
            .map_err(FetchError::Write)?;
    }
    for (index, file) in listed.iter().enumerate() {
        let index = u32::try_from(index).map_err(|_| FetchError::Invalid("too many files"))?;
        let path = local_path(folder, file.relative_path());
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(FetchError::Write)?;
        }
        let mut out = tokio::fs::File::create(&path)
            .await
            .map_err(FetchError::Write)?;
        fetch_file(
            connection,
            sequence,
            index,
            file,
            &mut out,
            STALL,
            still_wanted,
        )
        .await?;
        if file.modified_at_ms() > 0 {
            let modified = UNIX_EPOCH + Duration::from_millis(file.modified_at_ms());
            // Kept so offering the files on hands peers the same list; a
            // file without it is only fetched again.
            if let Err(error) = out.into_std().await.set_modified(modified) {
                debug!(%error, "Flow could not keep a received file's modification time");
            }
        }
    }
    Ok(())
}

/// Where `relative_path` goes under `folder`. It passed
/// [`is_safe_relative_path`], so every segment is a plain name.
fn local_path(folder: &Path, relative_path: &str) -> PathBuf {
    relative_path
        .split('/')
        .fold(folder.to_path_buf(), |path, segment| path.join(segment))
}

/// A new, empty transfer folder under `root`, named for now.
async fn new_folder(root: &Path) -> io::Result<PathBuf> {
    tokio::fs::create_dir_all(root).await?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    for attempt in 0..100_u32 {
        let name = if attempt == 0 {
            stamp.to_string()
        } else {
            format!("{stamp}-{attempt}")
        };
        let folder = root.join(name);
        match tokio::fs::create_dir(&folder).await {
            Ok(()) => return Ok(folder),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("no free name for a received-files folder"))
}

/// Removes a transfer folder that will not be used, off the async threads.
pub(super) fn discard(folder: &Path) {
    let folder = folder.to_owned();
    tokio::task::spawn_blocking(move || {
        if let Err(error) = fs::remove_dir_all(&folder) {
            debug!(%error, "Flow could not remove an unused received-files folder");
        }
    });
}

/// Removes all but the newest [`KEEP`] transfer folders under `root`. Only
/// folders named as [`new_folder`] names them are touched. Blocks.
pub(super) fn prune(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut transfers: Vec<((u128, u32), PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| Some((transfer_order(entry.file_name().to_str()?)?, entry.path())))
        .collect();
    transfers.sort_by_key(|(order, _)| std::cmp::Reverse(*order));
    for (_, folder) in transfers.into_iter().skip(KEEP) {
        if let Err(error) = fs::remove_dir_all(&folder) {
            debug!(%error, "Flow could not remove an old received-files folder");
        }
    }
}

/// When a transfer folder was made, from its name: `stamp` or
/// `stamp-attempt`.
fn transfer_order(name: &str) -> Option<(u128, u32)> {
    match name.split_once('-') {
        Some((stamp, attempt)) => Some((stamp.parse().ok()?, attempt.parse().ok()?)),
        None => Some((name.parse().ok()?, 0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn relative_paths(files: &[OfferedFile]) -> Vec<&str> {
        files
            .iter()
            .map(|file| file.relative_path.as_str())
            .collect()
    }

    #[test]
    fn copied_folders_are_walked_in_name_order() {
        let copied = tempfile::tempdir().unwrap();
        let root = copied.path();
        write(&root.join("notes.txt"), b"abc");
        write(&root.join("photos/b.jpg"), b"bb");
        write(&root.join("photos/2024/a.jpg"), b"a");
        write(&root.join("photos/a.png"), b"");
        fs::create_dir_all(root.join("photos/empty")).unwrap();
        fs::create_dir_all(root.join("drafts")).unwrap();

        let copied = collect(&[
            root.join("photos"),
            root.join("notes.txt"),
            root.join("drafts"),
        ])
        .unwrap();
        assert_eq!(copied.folders, ["photos/empty", "drafts"]);
        let files = copied.files;
        assert_eq!(
            relative_paths(&files),
            [
                "photos/2024/a.jpg",
                "photos/a.png",
                "photos/b.jpg",
                "notes.txt"
            ]
        );
        assert_eq!(files[2].size_bytes, 2);
        assert_eq!(files[2].source, root.join("photos").join("b.jpg"));
        assert!(files[0].modified_at_ms > 0);
    }

    #[test]
    fn copied_items_of_the_same_name_are_told_apart() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        write(&first.path().join("report.txt"), b"one");
        write(&second.path().join("Report.txt"), b"two");
        write(&second.path().join("data/x"), b"x");
        write(&first.path().join("data/y"), b"y");

        let copied = collect(&[
            first.path().join("report.txt"),
            second.path().join("Report.txt"),
            second.path().join("data"),
            first.path().join("data"),
        ])
        .unwrap();
        assert_eq!(
            relative_paths(&copied.files),
            ["report.txt", "Report (2).txt", "data/x", "data (2)/y"]
        );
    }

    #[test]
    fn more_files_than_the_limit_are_not_collected() {
        let copied = tempfile::tempdir().unwrap();
        for index in 0..=MAX_FILES {
            write(&copied.path().join(format!("many/{index}.txt")), b"");
        }
        assert!(matches!(
            collect(&[copied.path().join("many")]),
            Err(CollectError::TooLarge)
        ));
    }

    #[test]
    fn a_name_another_computer_cannot_create_is_not_sent() {
        assert_eq!(
            unique_name("a.txt", &mut HashSet::from(["a.txt".to_owned()])),
            "a (2).txt"
        );
        assert_eq!(unique_name(".profile", &mut HashSet::new()), ".profile");
        assert_eq!(
            unique_name(".profile", &mut HashSet::from([".profile".to_owned()])),
            ".profile (2)"
        );
        assert!(is_safe_relative_path("data (2)/y"));
    }

    #[test]
    fn only_the_newest_transfers_are_kept() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "1700000000000",
            "1700000000500",
            "1700000000500-1",
            "1600000000000",
            "unrelated",
        ] {
            fs::create_dir(root.path().join(name)).unwrap();
        }
        prune(root.path());
        let mut left: Vec<String> = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["1700000000500", "1700000000500-1", "unrelated"]);
    }
}
