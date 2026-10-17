use std::path::Path;

use serde::Deserialize;

use super::{InstallationSource, same_path};

#[cfg(target_os = "windows")]
pub(super) fn detect(executable: &Path) -> InstallationSource {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WOW64_64KEY};

    // Both shipped architectures use 64-bit MSI components. A machine-wide
    // install or another user's registration cannot own this per-user copy.
    let location = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(r"Software\OpenLogi", KEY_READ | KEY_WOW64_64KEY)
        .ok()
        .filter(|key| key.get_value::<u32, _>("Installed").ok() == Some(1))
        .and_then(|key| key.get_value::<String, _>("InstallLocation").ok());
    detect_in(executable, location.as_deref().map(Path::new))
}

fn detect_in(executable: &Path, msi_location: Option<&Path>) -> InstallationSource {
    // An MSI registration wins over a portable marker left in its directory.
    if msi_location.is_some_and(|location| {
        location.is_absolute() && same_path(&location.join("OpenLogi.exe"), executable)
    }) {
        return InstallationSource::WindowsMsi;
    }
    let Some(directory) = executable.parent() else {
        return InstallationSource::Unknown;
    };
    if same_path(&directory.join("OpenLogi.exe"), executable)
        && std::fs::read(directory.join("openlogi-installation.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Marker>(&bytes).ok())
            .is_some_and(|marker| marker.source == PortableSource::WindowsPortable)
    {
        return InstallationSource::WindowsPortable;
    }
    InstallationSource::Unknown
}

#[derive(Deserialize)]
struct Marker {
    source: PortableSource,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum PortableSource {
    WindowsPortable,
}

#[cfg(test)]
mod tests;
