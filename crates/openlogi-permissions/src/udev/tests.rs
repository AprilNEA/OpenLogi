//! The root half's file handling on scratch directories, and how the user
//! half reads pkexec's answer — no root, polkit, or udev involved.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

use super::{GrantError, RULES, RULES_FILE, STAGING_FILE, grant_outcome, is_install_request};

/// A symlink at the destination is replaced, not followed: the root half must
/// never write through a link into some other file. A staging file left by a
/// killed run does not block the next one.
#[test]
fn writing_replaces_a_symlink_instead_of_following_it() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let victim = dir.path().join("victim");
    fs::write(&victim, "untouched").expect("victim file");
    let destination = dir.path().join(RULES_FILE);
    symlink(&victim, &destination).expect("planted symlink");
    fs::write(dir.path().join(STAGING_FILE), "half-written").expect("stale staging file");

    super::write_rules(dir.path()).expect("rules written");

    assert_eq!(fs::read_to_string(&victim).expect("victim"), "untouched");
    let metadata = fs::symlink_metadata(&destination).expect("destination");
    assert!(metadata.is_file(), "the destination is a regular file now");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o644);
    assert_eq!(fs::read_to_string(&destination).expect("rules"), RULES);
    assert!(
        !dir.path().join(STAGING_FILE).exists(),
        "staging file renamed away"
    );
}

/// An identical file — what every package installs — is left as it is, while
/// a different one is replaced.
#[test]
fn writing_leaves_an_identical_file_in_place() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let destination = dir.path().join(RULES_FILE);
    fs::write(&destination, RULES).expect("packaged rules");
    let packaged = fs::metadata(&destination).expect("packaged").ino();

    super::write_rules(dir.path()).expect("nothing to write");
    assert_eq!(fs::metadata(&destination).expect("rules").ino(), packaged);

    fs::write(&destination, "# edited\n").expect("edited rules");
    super::write_rules(dir.path()).expect("rules rewritten");
    assert_eq!(fs::read_to_string(&destination).expect("rules"), RULES);
}

/// pkexec reserves 126 for a dismissed dialog and 127 for refusing; every
/// other status is the root half's. A failure carries the first stderr line,
/// without the prefix pkexec or `main` leads it with.
#[test]
fn pkexec_exit_status_maps_to_the_grant_outcome() {
    let failed = |reason: &str| Err(GrantError::Failed(reason.to_owned()));
    let cases = [
        (Some(0), "", Ok(())),
        (
            Some(126),
            "Error executing command as another user: Request dismissed\n",
            Err(GrantError::Cancelled),
        ),
        (
            Some(127),
            "Error executing command as another user: Not authorized\n\nThis incident has been reported.\n",
            failed("Not authorized"),
        ),
        (
            Some(1),
            "Error: `udevadm control --reload-rules` failed: Failed to send reload request\n",
            failed("`udevadm control --reload-rules` failed: Failed to send reload request"),
        ),
        (Some(1), "", failed("pkexec exited with status 1")),
        (None, "", failed("pkexec was terminated by a signal")),
    ];
    for (code, stderr, expected) in cases {
        assert_eq!(
            grant_outcome(code, stderr.as_bytes()),
            expected,
            "{code:?} {stderr:?}"
        );
    }
}

/// Only the exact root-half command line starts the root half; a deep link or
/// any extra argument is an ordinary launch.
#[test]
fn only_the_bare_install_argument_is_an_install_request() {
    let cases: [(&[&str], bool); 5] = [
        (&["openlogi-desktop", "--install-udev-rules"], true),
        (&["openlogi-desktop"], false),
        (&["openlogi-desktop", "openlogi://open-settings"], false),
        (
            &["openlogi-desktop", "--install-udev-rules", "--force"],
            false,
        ),
        (&["openlogi-desktop", "--", "--install-udev-rules"], false),
    ];
    for (args, expected) in cases {
        assert_eq!(
            is_install_request(args.iter().map(OsString::from)),
            expected,
            "{args:?}"
        );
    }
}
