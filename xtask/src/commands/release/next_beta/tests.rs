use anyhow::Result;
use xshell::{Shell, cmd};

use super::{NextBeta, Repo, next_beta, plan};

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

/// A Git repository in a temporary directory, for what `next_beta` reads
/// from Git, which `plan` cannot see.
///
/// Git runs without the user's and the system's configuration, so commit
/// signing or a global hooks path on the machine cannot change it.
struct Git {
    sh: Shell,
    _dir: tempfile::TempDir,
}

impl Git {
    /// An empty repository on `master`.
    fn init() -> Git {
        let git = Git::at(tempfile::tempdir().unwrap(), "repo");
        let sh = &git.sh;
        cmd!(sh, "git init -q -b master").run().unwrap();
        git
    }

    /// A shell in `dir/name`, with Git's configuration shut out.
    fn at(dir: tempfile::TempDir, name: &str) -> Git {
        let sh = Shell::new().unwrap();
        let config = dir.path().join("gitconfig");
        fs_err::write(&config, "").unwrap();
        sh.set_var("GIT_CONFIG_GLOBAL", &config);
        sh.set_var("GIT_CONFIG_NOSYSTEM", "1");
        for key in ["GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"] {
            sh.set_var(key, "xtask");
        }
        for key in ["GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"] {
            sh.set_var(key, "xtask@example.invalid");
        }
        let path = dir.path().join(name);
        fs_err::create_dir_all(&path).unwrap();
        sh.change_dir(&path);
        Git { sh, _dir: dir }
    }

    /// Write a root `Cargo.toml` with workspace version `version`.
    fn write_version(&self, version: &str) {
        let manifest = format!(
            "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"{version}\"\nrust-version = \"1.85\"\n"
        );
        fs_err::write(self.sh.current_dir().join("Cargo.toml"), manifest).unwrap();
    }

    /// Commit workspace version `version` with message `message`, and return
    /// the commit's SHA.
    fn commit(&self, version: &str, message: &str) -> String {
        self.write_version(version);
        let sh = &self.sh;
        cmd!(sh, "git add Cargo.toml").run().unwrap();
        cmd!(sh, "git commit -q --allow-empty -m {message}")
            .run()
            .unwrap();
        cmd!(sh, "git rev-parse HEAD").read().unwrap()
    }

    /// Commit the release PR's version `version` on a new branch `release`
    /// off the current commit, then go back to `master`.
    fn release_branch(&self, version: &str) {
        let sh = &self.sh;
        cmd!(sh, "git checkout -q -b release").run().unwrap();
        self.commit(version, "chore: release");
        cmd!(sh, "git checkout -q master").run().unwrap();
    }

    fn next_beta(&self, release_ref: &str) -> Result<(NextBeta, String)> {
        next_beta(&self.sh, release_ref)
    }
}

/// `master` two commits past `v0.8.17`, and a release PR preparing `0.8.18`
/// on top. Also returns `master`'s SHA.
fn two_past_v0_8_17() -> (Git, String) {
    let git = Git::init();
    git.commit("0.8.17", "chore: release v0.8.17");
    let sh = &git.sh;
    cmd!(sh, "git tag v0.8.17").run().unwrap();
    git.commit("0.8.17", "feat: one");
    let head = git.commit("0.8.17", "feat: two");
    git.release_branch("0.8.18");
    (git, head)
}

#[test]
fn a_beta_is_read_from_the_head_commit() {
    let (git, head) = two_past_v0_8_17();
    let (next, commit) = git.next_beta("release").unwrap();
    assert_eq!(next.tag.to_string(), "v0.8.18-beta.2");
    assert_eq!(next.build.to_string(), "0.8.17.2");
    assert_eq!(commit, head);
}

#[test]
fn an_uncommitted_edit_does_not_change_the_beta() {
    let (git, head) = two_past_v0_8_17();
    git.write_version("0.8.18");
    let (next, commit) = git.next_beta("release").unwrap();
    assert_eq!(next.tag.to_string(), "v0.8.18-beta.2");
    assert_eq!(commit, head);
}

#[test]
fn an_uncommitted_edit_does_not_hide_a_committed_version_bump() {
    let (git, _) = two_past_v0_8_17();
    // `master` takes 0.8.18 without a `v0.8.18` tag, and the release PR moves
    // on to 0.8.19. Putting 0.8.17 back only in the working tree does not
    // make the commit a beta of 0.8.19.
    git.commit("0.8.18", "chore: bump without a tag");
    let sh = &git.sh;
    cmd!(sh, "git branch -q -D release").run().unwrap();
    git.release_branch("0.8.19");
    git.write_version("0.8.17");
    let error = format!("{:#}", git.next_beta("release").unwrap_err());
    assert!(
        error.contains("HEAD carries workspace version 0.8.18"),
        "{error}"
    );
}

#[test]
fn a_shallow_repository_is_refused() {
    // `v0.8.17`, then five commits on a branch that a merge brings in: six
    // commits lead from `v0.8.17` to `master`.
    let full = Git::init();
    full.commit("0.8.17", "chore: release v0.8.17");
    let sh = &full.sh;
    cmd!(sh, "git tag v0.8.17").run().unwrap();
    cmd!(sh, "git checkout -q -b topic").run().unwrap();
    for n in 1..=5 {
        full.commit("0.8.17", &format!("feat: {n}"));
    }
    cmd!(sh, "git checkout -q master").run().unwrap();
    cmd!(sh, "git merge -q --no-ff -m merge topic")
        .run()
        .unwrap();
    full.release_branch("0.8.18");
    let (next, _) = full.next_beta("release").unwrap();
    assert_eq!(next.tag.to_string(), "v0.8.18-beta.6");

    // Two levels deep, `v0.8.17` is still on the history, but four of the
    // topic commits are not, so a count would say 2.
    let origin = full.sh.current_dir();
    let shallow = Git::at(tempfile::tempdir().unwrap(), "shallow");
    let sh = &shallow.sh;
    cmd!(sh, "git clone -q --no-local --depth 2 --no-tags {origin} .")
        .run()
        .unwrap();
    cmd!(sh, "git tag v0.8.17 HEAD^1").run().unwrap();
    cmd!(sh, "git fetch -q --depth 1 origin release")
        .run()
        .unwrap();
    let error = format!("{:#}", shallow.next_beta("FETCH_HEAD").unwrap_err());
    assert!(error.contains("this repository is shallow"), "{error}");
}
