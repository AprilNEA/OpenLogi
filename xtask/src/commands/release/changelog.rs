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
mod tests {
    use super::{latest_release_tag, release_version, strip_version_section};

    #[test]
    fn latest_release_tag_ignores_non_release_shapes() {
        let tags = "v0.7.10\nv0.7.4\nv0.7.4-rc.1\nv1\nvnext\n";
        assert_eq!(latest_release_tag(tags).as_deref(), Some("v0.7.10"));
    }

    #[test]
    fn latest_release_tag_orders_numerically_not_lexically() {
        // `sort` without -V puts v0.7.9 last; the changelog range would then
        // start after the newest release and come out empty.
        assert_eq!(
            latest_release_tag("v0.7.9\nv0.7.10\n").as_deref(),
            Some("v0.7.10")
        );
    }

    #[test]
    fn latest_release_tag_is_none_without_a_release_tag() {
        assert_eq!(latest_release_tag("vnext\n"), None);
        assert_eq!(latest_release_tag(""), None);
    }

    #[test]
    fn release_version_rejects_extra_fields() {
        assert_eq!(release_version("v1.2.3"), Some([1, 2, 3]));
        assert_eq!(release_version("v1.2.3.4"), None);
        assert_eq!(release_version("1.2.3"), None);
    }

    #[test]
    fn strip_version_section_removes_only_that_section() {
        let changelog = "\
# Changelog

## [0.8.0](https://example.invalid) - 2026-08-21

### Added

- a thing

## [0.7.4](https://example.invalid) - 2026-08-01

- an older thing
";
        let stripped = strip_version_section(changelog, "0.8.0").expect("0.8.0 section is present");
        assert_eq!(
            stripped,
            "\
# Changelog

## [0.7.4](https://example.invalid) - 2026-08-01

- an older thing
"
        );
    }

    #[test]
    fn strip_version_section_removes_a_trailing_section() {
        let changelog = "# Changelog\n\n## [0.8.0] - 2026-08-21\n\n- only entry\n";
        let stripped = strip_version_section(changelog, "0.8.0").expect("0.8.0 section is present");
        assert_eq!(stripped, "# Changelog\n\n");
    }

    #[test]
    fn strip_version_section_is_none_when_absent() {
        let changelog = "# Changelog\n\n## [0.7.4] - 2026-08-01\n\n- a thing\n";
        assert_eq!(strip_version_section(changelog, "0.8.0"), None);
    }

    #[test]
    fn strip_version_section_does_not_match_a_version_prefix() {
        // `## [0.8.0]` must not be found by a search for `0.8`.
        let changelog = "# Changelog\n\n## [0.8.0] - 2026-08-21\n\n- a thing\n";
        assert_eq!(strip_version_section(changelog, "0.8"), None);
    }
}
