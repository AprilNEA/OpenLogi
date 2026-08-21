use std::collections::HashMap;

use strum::IntoEnumIterator as _;

use super::{GROUPS, Job};
use crate::commands::ci::Host;

#[test]
fn every_name_resolves_to_its_own_job() {
    for job in Job::iter() {
        for name in job.names() {
            assert_eq!(
                Job::resolve(name).as_deref(),
                Some(&[job][..]),
                "name {name}"
            );
        }
    }
}

#[test]
fn names_are_unique_across_jobs() {
    let mut owners: HashMap<&str, Job> = HashMap::new();
    for job in Job::iter() {
        for name in job.names() {
            if let Some(other) = owners.insert(name, job) {
                panic!("name {name} is claimed by both {other:?} and {job:?}");
            }
        }
    }
    for (group, _) in GROUPS {
        assert!(
            !owners.contains_key(group),
            "group {group} also names a single job"
        );
    }
}

#[test]
fn matrix_leg_names_resolve() {
    // What someone copies out of a CI run's job list.
    for name in [
        "MSRV (cargo check, macos-latest)",
        "MSRV (cargo check, ubuntu-latest)",
    ] {
        assert_eq!(Job::resolve(name).as_deref(), Some(&[Job::Msrv][..]));
    }
    for name in ["tests (macos, arm64)", "tests (macos, x86_64)"] {
        assert_eq!(Job::resolve(name).as_deref(), Some(&[Job::TestsMacos][..]));
    }
}

#[test]
fn tests_names_both_test_jobs() {
    assert_eq!(
        Job::resolve("tests").as_deref(),
        Some(&[Job::TestsLinux, Job::TestsMacos][..])
    );
}

#[test]
fn unknown_job_names_do_not_resolve() {
    assert_eq!(Job::resolve("nightly"), None);
    assert_eq!(Job::resolve(""), None);
}

#[test]
fn the_default_run_is_the_ci_jobs_only() {
    // The focused suites are not jobs in ci.yml; a bare run must not claim
    // to have covered a pipeline job by running them.
    let default: Vec<Job> = Job::default_run().collect();
    for job in [Job::I18n, Job::Wire] {
        assert!(!default.contains(&job), "{job:?} is not a ci.yml job");
    }
    for job in Job::iter().filter(|job| ![Job::I18n, Job::Wire].contains(job)) {
        assert!(default.contains(&job), "{job:?} is a ci.yml job");
    }
}

/// The host lists are what decides a skip, so they are worth stating: on a
/// `cfg!` these were only ever evaluated on the host that made them true.
#[test]
fn jobs_name_the_hosts_ci_gives_them() {
    let hosts = |job: Job| job.spec().hosts.to_vec();
    assert_eq!(hosts(Job::TestsLinux), vec![Host::Linux]);
    assert_eq!(hosts(Job::TestsMacos), vec![Host::Macos]);
    // CI's msrv matrix is macos-latest + ubuntu-latest — there is no
    // Windows leg to reproduce.
    assert_eq!(hosts(Job::Msrv), vec![Host::Linux, Host::Macos]);
    // Natively on Windows, everywhere else as the cross-lint proxy.
    assert_eq!(hosts(Job::ClippyWindows), Host::ANY.to_vec());
}
