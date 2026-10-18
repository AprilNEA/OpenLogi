//! The machine's persistent Flow identity.
//!
//! Peers pin this key, so it must survive restarts. The PKCS#8 file is
//! created once, readable only by the user, and never replaced: an unreadable
//! file is an error, because a new key would silently break every pairing.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;

use openlogi_flow::transport::{IdentityError, MachineIdentity};
use thiserror::Error;

const FILE_NAME: &str = "flow-identity.pk8";

/// Why the Flow identity could not be loaded or created.
#[derive(Debug, Error)]
pub(crate) enum IdentityLoadError {
    /// No data directory to keep the identity in.
    #[error("no data directory for the Flow identity")]
    Paths(#[from] openlogi_core::paths::PathsError),
    /// Reading or writing the identity file failed.
    #[error("Flow identity file I/O failed")]
    Io(#[from] io::Error),
    /// The identity file does not hold a valid key.
    #[error("the Flow identity file does not hold a valid key")]
    Invalid(#[from] IdentityError),
}

/// Load the identity from the data directory, creating it on first use.
pub(crate) fn load() -> Result<MachineIdentity, IdentityLoadError> {
    load_at(&openlogi_core::paths::data_dir()?.join(FILE_NAME))
}

fn load_at(path: &Path) -> Result<MachineIdentity, IdentityLoadError> {
    match fs::read(path) {
        Ok(bytes) => {
            #[cfg(unix)]
            restrict(path)?;
            return Ok(MachineIdentity::from_pkcs8(bytes)?);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let identity = MachineIdentity::generate()?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(identity.private_key_pkcs8())?;
            file.sync_all()?;
            Ok(identity)
        }
        // Another agent created it first; use that one.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            Ok(MachineIdentity::from_pkcs8(fs::read(path)?)?)
        }
        Err(error) => Err(error.into()),
    }
}

/// Make an existing identity file readable only by its owner.
///
/// Unix only: on Windows the file inherits the per-user data directory's ACL.
#[cfg(unix)]
fn restrict(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(test)]
mod tests {
    use super::{IdentityLoadError, load_at};

    #[test]
    fn the_identity_is_created_once_and_kept_private() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("nested").join("flow-identity.pk8");
        let first = load_at(&path).expect("create the identity");
        let second = load_at(&path).expect("reload the identity");
        assert_eq!(first.public_key(), second.public_key());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn an_invalid_identity_is_reported_and_left_in_place() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("flow-identity.pk8");
        std::fs::write(&path, b"not a key").expect("write the file");
        assert!(matches!(load_at(&path), Err(IdentityLoadError::Invalid(_))));
        assert_eq!(std::fs::read(&path).expect("read the file"), b"not a key");
    }
}
