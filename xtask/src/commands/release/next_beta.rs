//! `release next-beta`: the beta tag to cut on `HEAD`, checked the way
//! `release version` checks it once the tag is pushed.
//!
//! A beta tags `master` between releases. Its version is the stable release
//! the open release PR prepares, and its number is the count of commits since
//! the stable release before it. The prerelease workflow runs this on
//! `master`, then creates the tag it prints.
//!
//! It reads the `HEAD` commit and the tags, not the working tree, so an
//! uncommitted edit cannot change the tag it names. It needs the whole history
//! and every `v*` tag: it refuses a shallow repository, because a shallow
//! history can hide commits from the count.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail, ensure};
use clap::Parser;
use semver::Version;
use xshell::{Shell, cmd};

use crate::support::github_output;
use crate::support::manifest::parse_workspace_package;
use crate::support::release_tag::{BetaBase, BuildVersion, ReleaseTag};
use openlogi_core::update_feed::UpdateChannel;

#[derive(Parser)]
pub(crate) struct Args {
    /// A commit of the open release PR. Its root `Cargo.toml` names the
    /// stable release the beta leads to.
    #[arg(long)]
    release_ref: String,
}

/// What the repository says, as [`plan`] reads it.
#[derive(Debug, Default)]
pub(crate) struct Repo {
    /// The release PR's workspace version: the stable release the beta leads
    /// to.
    pub(crate) release: String,
    /// The workspace version committed on `HEAD`.
    pub(crate) workspace: String,
    /// The `v*` tags reachable from `HEAD`.
    pub(crate) merged: Vec<String>,
    /// Every `v*` tag.
    pub(crate) tags: Vec<String>,
    /// The `v*` tags on `HEAD` itself.
    pub(crate) on_head: Vec<String>,
}

/// The beta to cut.
#[derive(Debug)]
pub(crate) struct NextBeta {
    pub(crate) tag: ReleaseTag,
    pub(crate) base: BetaBase,
    pub(crate) build: BuildVersion,
}

/// Print the beta's `tag`, `version`, `build_version`, `stable_base` and
/// `commit` as `key=value` lines, and append them to the step's GitHub
/// Actions outputs. `commit` is `HEAD`, the commit to tag.
///
/// Fails when [`plan`] does, without creating anything.
pub(crate) fn run(args: &Args) -> Result<()> {
    let sh = Shell::new()?;
    let root = PathBuf::from(cmd!(sh, "git rev-parse --show-toplevel").quiet().read()?);
    sh.change_dir(&root);
    let (next, head) = next_beta(&sh, &args.release_ref)?;

    let tag = next.tag.to_string();
    let version = next.tag.version().to_string();
    let build = next.build.to_string();
    let outputs = [
        ("tag", tag.as_str()),
        ("version", version.as_str()),
        ("build_version", build.as_str()),
        ("stable_base", next.base.tag.as_str()),
        ("commit", head.as_str()),
    ];
    for (key, value) in outputs {
        println!("{key}={value}");
    }
    github_output::write(&outputs)
}

/// The beta to cut on the `HEAD` commit of the repository `sh` is in, and that
/// commit's SHA.
///
/// It resolves `HEAD` once and reads everything else from that SHA: the
/// committed manifest, the tags on it and before it, and the commit count. The
/// working tree and a `HEAD` that moves later do not change the answer.
fn next_beta(sh: &Shell, release_ref: &str) -> Result<(NextBeta, String)> {
    let shallow = cmd!(sh, "git rev-parse --is-shallow-repository")
        .quiet()
        .read()?;
    ensure!(
        shallow.trim() != "true",
        "this repository is shallow, so its history can hide commits that a beta's number counts; fetch the whole history and every tag first (`git fetch --unshallow --tags`)"
    );

    let head_commit = "HEAD^{commit}";
    let head = cmd!(sh, "git rev-parse --verify {head_commit}")
        .quiet()
        .read()?;
    let lines = |text: String| -> Vec<String> { text.lines().map(str::to_owned).collect() };
    let repo = Repo {
        release: workspace_version(sh, release_ref).context("reading the release PR's version")?,
        workspace: workspace_version(sh, &head).context("reading HEAD's version")?,
        merged: lines(
            cmd!(sh, "git tag --merged {head} --list v*")
                .quiet()
                .read()?,
        ),
        tags: lines(cmd!(sh, "git tag --list v*").quiet().read()?),
        on_head: lines(
            cmd!(sh, "git tag --points-at {head} --list v*")
                .quiet()
                .read()?,
        ),
    };
    let next = plan(&repo, |base| {
        let range = format!("{base}..{head}");
        let count = cmd!(sh, "git rev-list --count {range}").quiet().read()?;
        count
            .trim()
            .parse()
            .with_context(|| format!("`git rev-list --count {range}` printed {count:?}"))
    })?;
    Ok((next, head))
}

/// The `[workspace.package]` version in the root `Cargo.toml` that `commit`
/// records.
fn workspace_version(sh: &Shell, commit: &str) -> Result<String> {
    let manifest = format!("{commit}:Cargo.toml");
    let text = cmd!(sh, "git show {manifest}").quiet().read()?;
    let package = parse_workspace_package(&text).with_context(|| format!("parsing {manifest}"))?;
    Ok(package.version)
}

/// The beta to cut on `HEAD`. `commits_since` counts the commits from a tag to
/// `HEAD`.
///
/// The tag is `v<release>-beta.<N>`, with `N` the commits since the newest
/// stable release on `HEAD`. Fails unless:
///
/// - the release is `X.Y.Z` and above that stable release;
/// - `HEAD` still carries that stable release's workspace version, and at
///   least one commit leads to it from there;
/// - `HEAD` has no beta yet, and the tag does not exist;
/// - the tag is above every beta, and the newest beta is on `HEAD`'s
///   history;
/// - its build version fits an MSI `ProductVersion`.
///
/// `release version` checks the first, second and last again on the pushed
/// tag. They check the tag's version, so a build, signature or publish can
/// still fail. The others keep betas rising, one per commit.
pub(crate) fn plan(
    repo: &Repo,
    commits_since: impl FnOnce(&str) -> Result<u64>,
) -> Result<NextBeta> {
    let release = Version::parse(&repo.release).with_context(|| {
        format!(
            "the release PR's version {:?} is not a semantic version",
            repo.release
        )
    })?;
    let first = ReleaseTag::beta(&release, 1)?;
    let Some(base) = first
        .beta_base(&repo.merged)
        .with_context(|| format!("the release PR prepares {release}"))?
    else {
        bail!("{first} is a beta, so it follows a stable release");
    };

    let expected = first.tagged_workspace_version(Some(&base));
    ensure!(
        repo.workspace == expected.to_string(),
        "HEAD carries workspace version {}, but {} is the newest stable release on it; a beta tags `master` between releases, so tag {} first",
        repo.workspace,
        base.tag,
        repo.workspace
    );

    let commits = commits_since(&base.tag)?;
    ensure!(
        commits >= 1,
        "HEAD is {}; a beta needs at least one commit after it",
        base.tag
    );
    let tag = ReleaseTag::beta(&release, commits)?;

    if let Some(beta) = repo.on_head.iter().find(|name| is_beta(name)) {
        bail!("HEAD is already {beta}; a commit gets one beta");
    }
    ensure!(
        !repo.tags.iter().any(|name| *name == tag.to_string()),
        "{tag} exists already"
    );
    if let Some((newest, name)) = repo
        .tags
        .iter()
        .filter_map(|name| Some((ReleaseTag::parse(name).ok()?, name)))
        .filter(|(parsed, _)| parsed.channel() == UpdateChannel::Beta)
        .max_by(|(left, _), (right, _)| left.version().cmp(right.version()))
    {
        ensure!(
            newest.version() < tag.version(),
            "{tag} is not above {newest}, the newest beta; betas only rise"
        );
        ensure!(
            repo.merged.contains(name),
            "{newest}, the newest beta, is not on HEAD's history"
        );
    }

    let build = BuildVersion::of(&tag, Some((&base, commits)))?;
    Ok(NextBeta { tag, base, build })
}

fn is_beta(name: &str) -> bool {
    ReleaseTag::parse(name).is_ok_and(|tag| tag.channel() == UpdateChannel::Beta)
}

#[cfg(test)]
mod tests;
