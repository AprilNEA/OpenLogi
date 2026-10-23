use anyhow::Result;

use super::{NextBeta, Repo, plan};

fn tags(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// A `master` five commits past `v0.8.17`, with the release PR preparing
/// `0.8.18` and no beta yet.
fn repo() -> Repo {
    Repo {
        release: "0.8.18".to_owned(),
        workspace: "0.8.17".to_owned(),
        merged: tags(&["v0.8.16", "v0.8.17"]),
        tags: tags(&["v0.8.16", "v0.8.17"]),
        on_head: Vec::new(),
    }
}

fn cut(repo: &Repo, commits: u64) -> Result<NextBeta> {
    plan(repo, |base| {
        assert_eq!(
            base, "v0.8.17",
            "commits are counted from the stable release on HEAD"
        );
        Ok(commits)
    })
}

fn refusal(repo: &Repo, commits: u64) -> String {
    match cut(repo, commits) {
        Ok(next) => panic!("cut {} where it should refuse", next.tag),
        Err(error) => format!("{error:#}"),
    }
}

#[test]
fn a_beta_is_the_release_prs_version_numbered_by_commits() {
    let next = cut(&repo(), 5).unwrap();
    assert_eq!(next.tag.to_string(), "v0.8.18-beta.5");
    assert_eq!(next.base.tag, "v0.8.17");
    assert_eq!(next.build.to_string(), "0.8.17.5");
}

#[test]
fn a_later_beta_rises_above_the_last() {
    let mut repo = repo();
    repo.merged.push("v0.8.18-beta.3".to_owned());
    repo.tags.push("v0.8.18-beta.3".to_owned());
    // Tags that are not release tags do not count as betas.
    repo.tags.push("v0.8.18-rc.9".to_owned());
    assert_eq!(cut(&repo, 7).unwrap().tag.to_string(), "v0.8.18-beta.7");
}

#[test]
fn betas_of_an_older_cycle_do_not_hold_the_next_one_back() {
    let mut repo = repo();
    for beta in ["v0.8.17-beta.40", "v0.8.16-beta.2"] {
        repo.merged.push(beta.to_owned());
        repo.tags.push(beta.to_owned());
    }
    assert_eq!(cut(&repo, 1).unwrap().tag.to_string(), "v0.8.18-beta.1");
}

#[test]
fn the_release_must_be_a_stable_version_above_the_last_stable_release() {
    for (release, expected) in [
        ("0.8.17", "does not lead past v0.8.17"),
        ("0.8.16", "does not lead past v0.8.17"),
        ("0.8.18-beta.1", "is not `X.Y.Z`"),
        ("0.8.18+meta", "is not `X.Y.Z`"),
        ("0.8", "is not a semantic version"),
    ] {
        let mut repo = repo();
        repo.release = release.to_owned();
        let error = refusal(&repo, 5);
        assert!(error.contains(expected), "{release}: {error}");
    }
}

#[test]
fn a_beta_needs_a_stable_release_on_head() {
    let mut repo = repo();
    repo.merged.clear();
    let error = plan(&repo, |_| -> Result<u64> {
        panic!("nothing to count from")
    })
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("no stable release tag is reachable"),
        "{error:#}"
    );
}

#[test]
fn a_bumped_but_untagged_master_gets_no_beta() {
    let mut repo = repo();
    repo.workspace = "0.8.18".to_owned();
    let error = refusal(&repo, 5);
    assert!(
        error.contains("HEAD carries workspace version 0.8.18"),
        "{error}"
    );
    assert!(error.contains("tag 0.8.18 first"), "{error}");
}

#[test]
fn the_stable_release_itself_gets_no_beta() {
    let error = refusal(&repo(), 0);
    assert!(error.contains("HEAD is v0.8.17"), "{error}");
}

#[test]
fn a_commit_gets_one_beta() {
    let mut repo = repo();
    repo.merged.push("v0.8.18-beta.5".to_owned());
    repo.tags.push("v0.8.18-beta.5".to_owned());
    repo.on_head.push("v0.8.18-beta.5".to_owned());
    let error = refusal(&repo, 5);
    assert!(error.contains("HEAD is already v0.8.18-beta.5"), "{error}");
}

#[test]
fn an_existing_tag_is_not_cut_again() {
    let mut repo = repo();
    // Same count on another line of history, for example a rewritten master.
    repo.tags.push("v0.8.18-beta.5".to_owned());
    let error = refusal(&repo, 5);
    assert!(error.contains("v0.8.18-beta.5 exists already"), "{error}");
}

#[test]
fn betas_only_rise() {
    let mut repo = repo();
    // The release PR went from 0.9.0 back to 0.8.18 after a 0.9.0 beta.
    repo.merged.push("v0.9.0-beta.4".to_owned());
    repo.tags.push("v0.9.0-beta.4".to_owned());
    let error = refusal(&repo, 5);
    assert!(error.contains("is not above v0.9.0-beta.4"), "{error}");
}

#[test]
fn the_newest_beta_must_be_on_heads_history() {
    let mut repo = repo();
    repo.tags.push("v0.8.18-beta.3".to_owned());
    let error = refusal(&repo, 5);
    assert!(
        error.contains("v0.8.18-beta.3, the newest beta, is not on HEAD's history"),
        "{error}"
    );
}

#[test]
fn a_beta_number_past_the_msi_limit_is_refused() {
    let error = refusal(&repo(), 65_536);
    assert!(
        error.contains("does not fit an MSI ProductVersion"),
        "{error}"
    );
}
