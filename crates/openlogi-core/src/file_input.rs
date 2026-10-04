//! Reject special-file inputs before a reader can wait on a pipe or device.

use std::{fs, io, path::Path};

/// The source may follow symlinks; atomic replacement must preserve target links.
#[derive(Clone, Copy)]
pub(crate) enum FileInput {
    Source,
    ReplacementTarget,
}

impl FileInput {
    /// Open a regular file without waiting for a Unix FIFO writer.
    pub(crate) fn open(self, path: &Path) -> io::Result<fs::File> {
        if matches!(self, Self::ReplacementTarget) && fs::symlink_metadata(path)?.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing to replace a symbolic-link target",
            ));
        }
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            // Reject a target link introduced after the path check, and inspect
            // the file actually opened instead of trusting earlier metadata.
            let flags = libc::O_NONBLOCK
                | if matches!(self, Self::ReplacementTarget) {
                    libc::O_NOFOLLOW
                } else {
                    0
                };
            options.custom_flags(flags);
        }
        let file = options.open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a regular input file",
            ));
        }
        Ok(file)
    }
}

#[cfg(test)]
#[cfg(unix)]
pub(crate) mod tests {
    use std::{ffi::CString, io, os::unix::ffi::OsStrExt as _, path::Path};

    /// Avoid spawning a child that can briefly inherit another test's file lock.
    #[expect(unsafe_code, reason = "create an actual FIFO fixture without forking")]
    pub(crate) fn create_fifo(path: &Path) {
        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: path is a live, NUL-terminated string; mkfifo retains no pointer.
        let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
        assert_eq!(result, 0, "create FIFO: {}", io::Error::last_os_error());
    }
}
