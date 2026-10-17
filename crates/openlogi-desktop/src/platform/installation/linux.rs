use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt as _;
use tokio::process::Command;

use super::{InstallationSource, LinuxPackage};

#[cfg(target_os = "linux")]
pub(super) fn detect(executable: &Path) -> InstallationSource {
    let Ok(runtime) = openlogi_core::worker::runtime() else {
        return InstallationSource::Unknown;
    };
    runtime.block_on(detect_with(executable, query))
}

async fn detect_with(
    executable: &Path,
    mut query: impl AsyncFnMut(Command) -> Option<String>,
) -> InstallationSource {
    if executable.starts_with("/nix/store") {
        return InstallationSource::Nix;
    }
    let mut dpkg = Command::new("dpkg-query");
    dpkg.args(["--search", "--"]).arg(executable);
    if let Some(output) = query(dpkg).await
        && let Some(package) = deb_owner(&output, executable)
    {
        // --search also knows files left by removed packages. A receipt alone
        // must not reclassify a manually copied binary as an installed package.
        let mut status = Command::new("dpkg-query");
        status.args(["--show", "--showformat=${db:Status-Status}", "--", package]);
        if query(status).await.as_deref() == Some("installed") {
            return InstallationSource::LinuxPackage(LinuxPackage::Deb);
        }
    }

    let mut rpm = Command::new("rpm");
    rpm.args(["--query", "--file", "--queryformat", "%{NAME}", "--"])
        .arg(executable);
    if query(rpm).await.as_deref() == Some("openlogi") {
        return InstallationSource::LinuxPackage(LinuxPackage::Rpm);
    }

    let mut pacman = Command::new("pacman");
    pacman.args(["-Qqo", "--"]).arg(executable);
    if query(pacman).await.as_deref() == Some("openlogi") {
        return InstallationSource::LinuxPackage(LinuxPackage::Arch);
    }
    InstallationSource::Unknown
}

async fn query(mut command: Command) -> Option<String> {
    command
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let read = async {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await?;
        child.wait().await.map(|status| (status, bytes))
    };
    let Ok(Ok((status, bytes))) = tokio::time::timeout(Duration::from_secs(2), read).await else {
        // kill() also waits/reaps. kill_on_drop alone can leave a zombie
        // when this one-shot probe's runtime shuts down immediately after.
        if let Err(error) = child.kill().await {
            tracing::debug!(%error, "could not reap installation query");
        }
        return None;
    };
    if !status.success() {
        return None;
    }
    String::from_utf8(bytes)
        .ok()
        .map(|text| text.trim().to_owned())
}

fn deb_owner<'a>(output: &'a str, executable: &Path) -> Option<&'a str> {
    output.lines().find_map(|line| {
        let (owner, path) = line.rsplit_once(": ")?;
        // dpkg accepts glob patterns; only an exact path match proves ownership.
        // Multiarch package names are allowed, but lists of owners are not.
        (owner.split(':').next() == Some("openlogi")
            && !owner.contains([',', ' '])
            && Path::new(path) == executable)
            .then_some(owner)
    })
}

#[cfg(test)]
mod tests;
