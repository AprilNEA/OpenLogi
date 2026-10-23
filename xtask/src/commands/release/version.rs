//! `release version`: what a release tag says, checked against the commit it
//! names.

use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};
use clap::Parser;
use xshell::{Shell, cmd};

use crate::support::github_output;
use crate::support::manifest::workspace_package;
use crate::support::release_tag::{BuildVersion, ReleaseTag};

#[derive(Parser)]
pub(crate) struct Args {
    /// Release tag, for example `v0.8.17` or `v0.8.17-beta.12`. It must be
    /// checked out.
    #[arg(long, env = "GITHUB_REF_NAME")]
    tag: String,
}

/// Print the tag's `channel`, `version`, `build_version`, `stamp_version` and
/// `previous_tag` as `key=value` lines, and append them to the step's GitHub
/// Actions outputs. `stamp_version` is [`ReleaseTag::stamp_version`], empty
/// for a stable tag. `previous_tag` is [`ReleaseTag::notes_base`], empty for
/// the first release.
///
/// Fails unless `HEAD` is the tagged commit and carries the workspace version
/// [`ReleaseTag::tagged_workspace_version`] names: a stable tag's own version,
/// or for a beta the stable release it follows. A beta's number must also be
/// the count of commits from that stable release.
pub(crate) fn run(args: &Args) -> Result<()> {
    let sh = Shell::new()?;
    let root = PathBuf::from(cmd!(sh, "git rev-parse --show-toplevel").quiet().read()?);
    sh.change_dir(&root);
    let tag = ReleaseTag::parse(&args.tag)?;

    let name = &args.tag;
    let tagged_commit = format!("{name}^{{commit}}");
    let head_commit = "HEAD^{commit}";
    let tagged = cmd!(sh, "git rev-parse --verify {tagged_commit}")
        .quiet()
        .read()?;
    let head = cmd!(sh, "git rev-parse --verify {head_commit}")
        .quiet()
        .read()?;
    ensure!(
        tagged == head,
        "{tag} is {tagged}, but HEAD is {head}; check the tag out first"
    );
    let merged: Vec<String> = cmd!(sh, "git tag --merged {name} --list v*")
        .quiet()
        .read()?
        .lines()
        .map(str::to_owned)
        .collect();
    let base = tag.beta_base(&merged)?;
    let workspace = workspace_package(&root)?.version;
    let expected = tag.tagged_workspace_version(base.as_ref());
    ensure!(
        workspace == expected.to_string(),
        "{tag} needs workspace version {expected} on the tagged commit, but it is {workspace}"
    );

    let beta = match &base {
        Some(base) => {
            let range = format!("{}..{name}", base.tag);
            let count = cmd!(sh, "git rev-list --count {range}").quiet().read()?;
            let commits: u64 = count
                .trim()
                .parse()
                .with_context(|| format!("`git rev-list --count {range}` printed {count:?}"))?;
            Some((base, commits))
        }
        None => None,
    };
    let stamp = tag
        .stamp_version(base.as_ref())
        .map(ToString::to_string)
        .unwrap_or_default();
    let build = BuildVersion::of(&tag, beta)?.to_string();
    let version = tag.version().to_string();
    let previous = tag.notes_base(&merged).unwrap_or_default();
    let outputs = [
        ("channel", tag.channel().as_str()),
        ("version", version.as_str()),
        ("build_version", build.as_str()),
        ("stamp_version", stamp.as_str()),
        ("previous_tag", previous),
    ];
    for (key, value) in outputs {
        println!("{key}={value}");
    }
    github_output::write(&outputs)
}
