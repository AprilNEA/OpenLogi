//! Which certificate the dev bundle is signed with, and remembering it.
//!
//! macOS ties an Accessibility or Input-Monitoring grant to the bundle's
//! designated requirement, which for a development certificate pins the leaf
//! cert. Signing the dev bundles with a *different* certificate therefore
//! silently voids every permission the developer granted them — and
//! `security find-identity` returns no guaranteed order, so picking the first
//! line is exactly how that happens on a machine with more than one Apple
//! Development identity.
//!
//! So the choice is remembered, outside `target/`: `cargo clean` wipes the
//! bundle but not the grants. It changes only when the remembered certificate
//! has left the keychain — an annual rotation — and that case is announced
//! rather than left looking like a mystery.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use openlogi_core::paths;
use xshell::{Shell, cmd};

use super::super::bundle::quoted_identity;

/// The prefix that marks a local development certificate.
const DEVELOPMENT: &str = "Apple Development:";

/// How the dev bundle gets signed.
pub(super) enum Signing {
    /// With a real development certificate, so grants survive a rebuild.
    Identity(String),
    /// No certificate is available. Ad-hoc signatures are identified by their
    /// own hash, so every rebuild is a new identity and the grants go with it.
    AdHoc,
    /// `OPENLOGI_DEV_CODESIGN=0`.
    Skipped,
}

impl Signing {
    /// Whether the executable must be copied into the bundle rather than
    /// hardlinked: `codesign` rewrites the Mach-O in place, and a hardlink
    /// would corrupt Cargo's own artifact behind its back.
    pub(super) const fn rewrites_binaries(&self) -> bool {
        !matches!(self, Self::Skipped)
    }

    /// Sign `targets` in the order given — nested helpers before the app that
    /// seals them, since an outer signature covers what is already inside.
    pub(super) fn run(&self, targets: &[PathBuf]) -> Result<()> {
        let identity = match self {
            Self::Identity(identity) => identity.as_str(),
            Self::AdHoc => "-",
            Self::Skipped => return Ok(()),
        };
        let sh = Shell::new()?;
        for target in targets {
            cmd!(
                sh,
                "codesign --force --sign {identity} --timestamp=none {target}"
            )
            .run()?;
        }
        Ok(())
    }
}

/// Decide how to sign, announcing anything that costs the developer a grant.
///
/// `app` is consulted when nothing is remembered yet: an existing bundle is
/// itself a record of the choice, and the one the current grants are tied to,
/// so inheriting it means introducing the state file cannot be the thing that
/// voids them.
pub(super) fn resolve(app: &Path) -> Result<Signing> {
    if std::env::var("OPENLOGI_DEV_CODESIGN").as_deref() == Ok("0") {
        return Ok(Signing::Skipped);
    }
    if let Ok(pinned) = std::env::var("OPENLOGI_DEV_CODESIGN_IDENTITY")
        && !pinned.trim().is_empty()
    {
        return Ok(Signing::Identity(pinned));
    }

    let available = development_identities()?;
    let Some(first) = available.first() else {
        println!(
            "note: no Apple Development identity found — signing the dev bundle ad-hoc.\n      \
             Ad-hoc signatures are identified by their hash, so macOS voids the dev\n      \
             build's Accessibility grant on every rebuild."
        );
        return Ok(Signing::AdHoc);
    };

    let remembered = read_remembered()?.or_else(|| signed_with(app));
    if let Some(kept) = remembered.as_ref().filter(|it| available.contains(it)) {
        remember(kept)?;
        return Ok(Signing::Identity(kept.clone()));
    }

    if let Some(gone) = remembered {
        println!(
            "warning: the dev signing certificate changed.\n\n  \
             was: {gone}\n  now: {first}\n\n\
             macOS ties Accessibility and Input Monitoring grants to the signing\n\
             certificate, so the permissions you gave the dev build no longer apply.\n\
             Clear the stale entries and grant them once more:\n\n  \
             tccutil reset Accessibility {agent}\n  tccutil reset ListenEvent {agent}\n\n\
             Pin a specific certificate with OPENLOGI_DEV_CODESIGN_IDENTITY to stop this.",
            agent = openlogi_core::brand::dev_id(openlogi_core::brand::AGENT_ID),
        );
    } else if available.len() > 1 {
        println!(
            "note: several Apple Development identities are available; using \"{first}\".\n      \
             Set OPENLOGI_DEV_CODESIGN_IDENTITY to choose a different one — switching\n      \
             voids the dev build's Accessibility grant."
        );
    }
    remember(first)?;
    Ok(Signing::Identity(first.clone()))
}

/// Every development certificate in the keychain, sorted so that even a first
/// run on a fresh machine is deterministic.
fn development_identities() -> Result<Vec<String>> {
    let sh = Shell::new()?;
    let Ok(output) = cmd!(sh, "security find-identity -v -p codesigning").read() else {
        return Ok(Vec::new());
    };
    let mut identities: Vec<String> = output
        .lines()
        .filter_map(quoted_identity)
        .filter(|identity| identity.starts_with(DEVELOPMENT))
        .collect();
    identities.sort_unstable();
    Ok(identities)
}

/// The certificate an already-built bundle carries, if it is readable.
fn signed_with(app: &Path) -> Option<String> {
    let sh = Shell::new().ok()?;
    // `codesign -d` writes its report to stderr, which `read_stderr` collects.
    let report = cmd!(sh, "codesign -dv --verbose=2 {app}")
        .ignore_status()
        .read_stderr()
        .ok()?;
    report
        .lines()
        .filter_map(|line| line.strip_prefix("Authority="))
        .find(|authority| authority.starts_with(DEVELOPMENT))
        .map(str::to_owned)
}

/// Where the choice is kept: under the dev profile's config directory, which
/// `cargo clean` does not touch.
fn state_path() -> Result<PathBuf> {
    Ok(paths::xdg_config_home()
        .context("resolving the XDG config home")?
        .join(paths::DEV_APP_DIR)
        .join("codesign-identity"))
}

fn read_remembered() -> Result<Option<String>> {
    let path = state_path()?;
    match fs_err::read_to_string(&path) {
        Ok(contents) => Ok(Some(contents.trim().to_owned()).filter(|it| !it.is_empty())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn remember(identity: &str) -> Result<()> {
    let path = state_path()?;
    if let Some(parent) = path.parent() {
        fs_err::create_dir_all(parent)?;
    }
    fs_err::write(&path, identity)?;
    Ok(())
}
