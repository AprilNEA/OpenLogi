use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Parser;
use xshell::{Shell, cmd};

use crate::support::fs::ensure_command;
use crate::support::manifest::workspace_package;

#[derive(Parser)]
pub(crate) struct Args {
    /// git-cliff configuration.
    #[arg(long, default_value = "cliff.toml")]
    config: PathBuf,
    /// Changelog the new section is prepended to.
    #[arg(long, default_value = "CHANGELOG.md")]
    changelog: PathBuf,
}

/// Write the next workspace version's section into the changelog with
/// git-cliff: every conventional commit in the whole repo since the previous
/// `v*` tag, formatted by `cliff.toml`.
///
/// release-plz itself is package-path-scoped and skips the `release = false`
/// app crates, so it cannot produce this. The release-plz workflow runs this
/// command against the release PR's branch.
pub(crate) fn run(args: &Args) -> Result<()> {
    let sh = Shell::new()?;
    // The release-plz workflow runs this after `git checkout` swaps the tree to
    // the release branch, so the repository — not the build-time manifest path
    // — is what defines the root.
    let root = PathBuf::from(cmd!(sh, "git rev-parse --show-toplevel").read()?);
    sh.change_dir(&root);

    // Invoked below as the `git cliff` subcommand; the binary git resolves is
    // `git-cliff`.
    ensure_command("git-cliff")?;

    let version = workspace_package(&root)?.version;
    let tag = format!("v{version}");

    let tags = cmd!(sh, "git tag --list v*").read()?;
    let Some(last_tag) = latest_release_tag(&tags) else {
        bail!("no previous vX.Y.Z tag");
    };
    if last_tag == tag {
        bail!("workspace version {version} is already tagged as {tag}");
    }

    // Drop a stale section for this version so re-runs — and every update to an
    // open release PR — stay idempotent.
    let changelog = root.join(&args.changelog);
    let text = fs_err::read_to_string(&changelog)?;
    if let Some(without_section) = strip_version_section(&text, &version) {
        fs_err::write(&changelog, without_section)?;
    }

    let range = format!("{last_tag}..");
    let config = &args.config;
    let changelog = &args.changelog;
    cmd!(
        sh,
        "git cliff {range} --config {config} --tag {tag} --prepend {changelog}"
    )
    .run()?;

    eprintln!("wrote {tag} changelog from {last_tag}..HEAD");
    Ok(())
}

/// The highest `vX.Y.Z` tag in `git tag --list` output.
///
/// Release tags are the only ones compared: anything else matching `v*` — a
/// pre-release, a moved pointer like `v1`, a typo — is not a released version
/// and must not become the changelog's lower bound.
fn latest_release_tag(tags: &str) -> Option<String> {
    tags.lines()
        .filter_map(|tag| Some((release_version(tag)?, tag)))
        .max()
        .map(|(_, tag)| tag.to_owned())
}

/// `v1.2.3` as its three numeric fields, or `None` for any other shape.
fn release_version(tag: &str) -> Option<[u64; 3]> {
    let mut fields = tag.strip_prefix('v')?.split('.');
    let version = [
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
    ];
    fields.next().is_none().then_some(version)
}

/// The changelog without its `## [version]` section, or `None` when it has no
/// such section.
///
/// The section runs to the next `## [` heading — the shape `cliff.toml`
/// generates — or to the end of the file for the newest entry.
fn strip_version_section(changelog: &str, version: &str) -> Option<String> {
    let heading = format!("## [{version}]");
    let start = changelog
        .lines()
        .scan(0, |offset, line| {
            let start = *offset;
            *offset += line.len() + 1;
            Some((start, line))
        })
        .find(|(_, line)| line.starts_with(&heading))?
        .0;
    let end = changelog[start + heading.len()..]
        .find("\n## [")
        .map_or(changelog.len(), |offset| start + heading.len() + offset + 1);
    Some(format!("{}{}", &changelog[..start], &changelog[end..]))
}

#[cfg(test)]
mod tests;
