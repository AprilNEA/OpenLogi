use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use xshell::{Shell, cmd};

use crate::support::manifest::{parse_workspace_package, workspace_package};

const RELEASE_BRANCH: &str = "master";
const RELEASE_UPSTREAM: &str = "origin/master";

#[derive(Debug, PartialEq, Eq)]
enum Resolution {
    AlreadyTagged,
    Commit(String),
}

/// Pin the release job to the commit that introduced the current workspace
/// version. release-plz requires an attached branch with an upstream.
pub(crate) fn run() -> Result<()> {
    let sh = Shell::new()?;
    let root = PathBuf::from(cmd!(sh, "git rev-parse --show-toplevel").read()?);
    sh.change_dir(&root);

    let version = workspace_package(&root)?.version;
    let tag = format!("v{version}");
    match resolve(&sh, &version)? {
        Resolution::AlreadyTagged => {
            eprintln!("tag {tag} already exists — nothing to release");
            write_skip_output(true)
        }
        Resolution::Commit(sha) => {
            eprintln!("checking out version-bump commit {sha} for {tag}");
            cmd!(sh, "git switch --force -C {RELEASE_BRANCH} {sha}").run()?;
            cmd!(
                sh,
                "git branch --set-upstream-to={RELEASE_UPSTREAM} {RELEASE_BRANCH}"
            )
            .run()?;
            write_skip_output(false)
        }
    }
}

fn resolve(sh: &Shell, version: &str) -> Result<Resolution> {
    let tag = format!("v{version}");
    let tags = cmd!(sh, "git tag --list {tag}").quiet().read()?;
    if tags.lines().any(|candidate| candidate == tag) {
        return Ok(Resolution::AlreadyTagged);
    }

    if let Some(sha) = release_subject_commit(sh, version)? {
        return Ok(Resolution::Commit(sha));
    }
    if let Some(sha) = manifest_version_commit(sh, version)? {
        return Ok(Resolution::Commit(sha));
    }
    bail!("could not locate the version-bump commit for {tag}")
}

fn release_subject_commit(sh: &Shell, version: &str) -> Result<Option<String>> {
    let expected = format!("chore: release v{version}");
    let expected_with_pr = format!("{expected} (");
    let log = cmd!(sh, "git log --format=%H%x09%s").quiet().read()?;
    Ok(log.lines().find_map(|line| {
        let (sha, subject) = line.split_once('\t')?;
        (subject == expected || subject.starts_with(&expected_with_pr)).then(|| sha.to_owned())
    }))
}

fn manifest_version_commit(sh: &Shell, version: &str) -> Result<Option<String>> {
    let version_line = r#"^version = ""#;
    let commits = cmd!(sh, "git log -G {version_line} --format=%H -- Cargo.toml")
        .quiet()
        .read()?;
    for sha in commits.lines() {
        let object = format!("{sha}:Cargo.toml");
        let Ok(manifest) = cmd!(sh, "git show {object}").quiet().read() else {
            continue;
        };
        let Ok(package) = parse_workspace_package(&manifest) else {
            continue;
        };
        if package.version == version {
            return Ok(Some(sha.to_owned()));
        }
    }
    Ok(None)
}

fn write_skip_output(skip: bool) -> Result<()> {
    let Some(path) = std::env::var_os("GITHUB_OUTPUT") else {
        return Ok(());
    };
    let mut output = fs_err::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| {
            format!(
                "opening GitHub Actions output {}",
                PathBuf::from(path).display()
            )
        })?;
    writeln!(output, "skip={skip}")?;
    Ok(())
}

#[cfg(test)]
mod tests;
