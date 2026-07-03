use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use path_absolutize::Absolutize;

pub(crate) fn repo_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .context("could not locate repository root from xtask manifest directory")
}

pub(crate) fn command_exists(name: &str) -> bool {
    which::which(name).is_ok()
}

pub(crate) fn ensure_command(name: &str) -> Result<()> {
    if command_exists(name) {
        Ok(())
    } else {
        bail!("{name} not found — run inside the devenv shell, or install it first")
    }
}

pub(crate) fn ensure_file(path: &Path) -> Result<()> {
    let metadata = fs_err::metadata(path)?;
    if metadata.is_file() {
        Ok(())
    } else {
        bail!("missing file {}", path.display())
    }
}

pub(crate) fn ensure_dir(path: &Path) -> Result<()> {
    let metadata = fs_err::metadata(path)?;
    if metadata.is_dir() {
        Ok(())
    } else {
        bail!("missing directory {}", path.display())
    }
}

pub(crate) fn absolutize(root: &Path, path: &Path) -> PathBuf {
    path.absolutize_from(root)
        .map_or_else(|_| root.join(path), std::borrow::Cow::into_owned)
}
