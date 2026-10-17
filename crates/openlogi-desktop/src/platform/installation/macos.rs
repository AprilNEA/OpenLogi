use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use super::{HomebrewCask, InstallationSource, same_path};

pub(super) fn detect(executable: &Path) -> InstallationSource {
    let mut prefixes = vec![PathBuf::from("/opt/homebrew"), PathBuf::from("/usr/local")];
    if let Some(prefix) = std::env::var_os("HOMEBREW_PREFIX") {
        prefixes.push(PathBuf::from(prefix));
    }
    // Finder's PATH usually omits brew; the defaults above still work. A
    // shell-launched app can also discover a nonstandard prefix without
    // invoking brew (which could update taps or make network requests).
    if let Some(path) = std::env::var_os("PATH") {
        prefixes.extend(std::env::split_paths(&path).filter_map(|bin| {
            (bin.file_name()? == "bin" && bin.join("brew").is_file())
                .then(|| bin.parent().map(Path::to_path_buf))?
        }));
    }
    detect_in(executable, &prefixes)
}

fn detect_in(executable: &Path, prefixes: &[PathBuf]) -> InstallationSource {
    let Some(bundle) = app_bundle(executable) else {
        return InstallationSource::Unknown;
    };
    let mut found = None;
    for prefix in prefixes.iter().filter(|prefix| prefix.is_absolute()) {
        for (token, cask) in [
            ("openlogi", HomebrewCask::Official),
            ("openlogi@latest", HomebrewCask::Latest),
        ] {
            if owns_bundle(&prefix.join("Caskroom").join(token), token, bundle) {
                if found.is_some_and(|previous| previous != cask) {
                    return InstallationSource::Unknown;
                }
                found = Some(cask);
            }
        }
    }
    found.map_or(
        InstallationSource::MacAppBundle,
        InstallationSource::Homebrew,
    )
}

fn app_bundle(executable: &Path) -> Option<&Path> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app"
        && contents.join("Info.plist").is_file())
    .then_some(bundle)
}

#[derive(Deserialize)]
struct Receipt {
    source: ReceiptSource,
}

#[derive(Deserialize)]
struct ReceiptSource {
    version: String,
}

fn owns_bundle(caskroom: &Path, token: &str, bundle: &Path) -> bool {
    let Ok(receipt) = std::fs::read(caskroom.join(".metadata/INSTALL_RECEIPT.json")) else {
        return false;
    };
    let Ok(receipt) = serde_json::from_slice::<Receipt>(&receipt) else {
        return false;
    };
    let version = Path::new(&receipt.source.version);
    // A version is one directory name, never a path supplied by the receipt.
    if !matches!(version.components().next(), Some(Component::Normal(_)))
        || version.components().count() != 1
    {
        return false;
    }
    let Ok(timestamps) = std::fs::read_dir(caskroom.join(".metadata").join(version)) else {
        return false;
    };
    let installed = timestamps.filter_map(Result::ok).any(|timestamp| {
        ["json", "internal.json", "rb"].iter().any(|extension| {
            timestamp
                .path()
                .join("Casks")
                .join(format!("{token}.{extension}"))
                .is_file()
        })
    });
    // Homebrew moves the app to appdir and leaves this symlink pointing back.
    // That establishes ownership even for --appdir, and unlike the receipt
    // version it stays valid after a cask's permitted in-app update.
    let backlink = caskroom.join(version).join("OpenLogi.app");
    installed && backlink.is_symlink() && same_path(&backlink, bundle)
}

#[cfg(test)]
mod tests;
