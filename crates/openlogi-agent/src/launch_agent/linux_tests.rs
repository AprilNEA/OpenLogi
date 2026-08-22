//! Unit tests for the Linux systemd user-unit reconcile.

use super::*;

#[test]
fn rendered_unit_targets_agent_and_restarts_on_failure() {
    let body = render_unit("/usr/bin/openlogi-agent");
    assert!(body.contains("ExecStart=/usr/bin/openlogi-agent"));
    assert!(body.contains("Restart=on-failure"));
    assert!(body.contains("WantedBy=graphical-session.target"));
    assert!(!body.contains("--minimized"));
}

#[test]
fn rendered_unit_is_valid_ini_with_all_three_sections() {
    let body = render_unit("/usr/bin/openlogi-agent");
    assert!(body.contains("[Unit]"));
    assert!(body.contains("[Service]"));
    assert!(body.contains("[Install]"));
}

#[test]
fn escape_systemd_exec_doubles_percent() {
    assert_eq!(
        escape_systemd_exec("/home/user%20/bin/openlogi-agent"),
        "/home/user%%20/bin/openlogi-agent"
    );
}

#[test]
fn escape_systemd_exec_quotes_path_with_spaces() {
    let result = escape_systemd_exec("/home/my user/bin/openlogi-agent");
    assert_eq!(result, "\"/home/my user/bin/openlogi-agent\"");
}

#[test]
fn escape_systemd_exec_quotes_and_doubles_percent_with_spaces() {
    let result = escape_systemd_exec("/home/my%20 user/openlogi-agent");
    assert_eq!(result, "\"/home/my%%20 user/openlogi-agent\"");
}

#[test]
fn escape_systemd_exec_doubles_dollar() {
    assert_eq!(
        escape_systemd_exec("/opt/release$1/bin/openlogi-agent"),
        "/opt/release$$1/bin/openlogi-agent"
    );
}

#[test]
fn escape_systemd_exec_plain_path_unchanged() {
    let path = "/usr/local/bin/openlogi-agent";
    assert_eq!(escape_systemd_exec(path), path);
}

#[test]
fn systemctl_arguments_render_as_a_command_suffix() {
    assert_eq!(
        SystemctlArgsDisplay(&["enable", UNIT_NAME]).to_string(),
        "enable openlogi-agent.service"
    );
}

#[test]
fn unit_path_uses_home_fallback() {
    // When XDG_CONFIG_HOME is unset (or relative), falls back to $HOME/.config.
    // We can't mutate global env safely in a parallel test suite, so we test
    // the logic indirectly: the path must end in the UNIT_NAME component.
    let path = generated_unit_path().expect("generated_unit_path resolves with a valid HOME");
    assert!(path.ends_with(UNIT_NAME));
    assert!(path.to_string_lossy().contains("systemd/user"));
}

/// The generated unit must sit in the data tier, which systemd ranks below
/// the user's own config directory. Writing to the config tier is the bug
/// this addresses: it outranks everything and cannot be overridden.
#[test]
fn generated_unit_lives_below_the_user_config_tier() {
    let generated = generated_unit_path().expect("generated path resolves");
    let legacy = legacy_unit_path().expect("legacy path resolves");
    let config_home = openlogi_core::paths::xdg_config_home().expect("config home resolves");

    assert!(
        !generated.starts_with(&config_home),
        "{}",
        generated.display()
    );
    assert!(legacy.starts_with(&config_home), "{}", legacy.display());
    assert_ne!(generated, legacy);
}

#[test]
fn a_rendered_unit_is_recognised_as_ours_for_any_executable() {
    for exe in [
        "/usr/bin/openlogi-agent",
        "/usr/local/bin/openlogi-agent",
        "/home/dev/OpenLogi/target/debug/openlogi-agent",
    ] {
        assert!(
            is_generated_unit(&render_unit(exe)),
            "{exe} should round-trip"
        );
    }
}

/// The escaped forms are the reason [`render_unit_with_exec`] exists: a
/// naive check that re-escapes the parsed value turns `%%` into `%%%%` and
/// fails to recognise a unit this app wrote.
#[test]
fn a_rendered_unit_is_recognised_as_ours_when_the_path_needs_escaping() {
    for exe in [
        "/opt/100%/bin/openlogi-agent",
        "/opt/release$1/bin/openlogi-agent",
        "/home/a b/openlogi-agent",
    ] {
        assert!(
            is_generated_unit(&render_unit(exe)),
            "{exe} should round-trip"
        );
    }
}

#[test]
fn a_hand_edited_unit_is_not_ours() {
    let base = render_unit("/usr/bin/openlogi-agent");

    let with_extra = base.replace(
        "Restart=on-failure",
        "Environment=RUST_LOG=debug\nRestart=on-failure",
    );
    assert!(
        !is_generated_unit(&with_extra),
        "an added directive is theirs"
    );

    let changed = base.replace("RestartSec=5", "RestartSec=1");
    assert!(!is_generated_unit(&changed), "a changed value is theirs");

    let removed = base.replace("After=graphical-session.target\n", "");
    assert!(
        !is_generated_unit(&removed),
        "a dropped directive is theirs"
    );

    assert!(!is_generated_unit(""), "an empty file has no ExecStart");
    assert!(
        !is_generated_unit("[Service]\nExecStart=/bin/true\n"),
        "an unrelated unit is theirs"
    );
}

/// Two `ExecStart` lines is not a shape the renderer can emit, so such a
/// file is the user's however much of it looks familiar.
#[test]
fn a_unit_with_two_exec_starts_is_not_ours() {
    let doubled = render_unit("/usr/bin/openlogi-agent").replace(
        "Restart=on-failure",
        "ExecStart=/bin/true\nRestart=on-failure",
    );
    assert_eq!(exec_start_value(&doubled), None);
    assert!(!is_generated_unit(&doubled));
}

#[test]
fn exec_start_value_is_returned_still_escaped() {
    let unit = render_unit("/opt/100%/bin/openlogi-agent");
    assert_eq!(
        exec_start_value(&unit),
        Some("/opt/100%%/bin/openlogi-agent")
    );
}

#[test]
fn unescaping_inverts_the_escaping() {
    for exe in [
        "/usr/bin/openlogi-agent",
        "/opt/100%/bin/openlogi-agent",
        "/opt/release$1/bin/openlogi-agent",
        "/home/a b/openlogi-agent",
        "/home/quote\"odd/openlogi-agent",
    ] {
        assert_eq!(unescape_systemd_exec(&escape_systemd_exec(exe)), exe);
    }
}

/// A packaged unit may carry arguments; only the program is compared.
#[test]
fn unescaping_drops_arguments() {
    assert_eq!(
        unescape_systemd_exec("/usr/bin/openlogi-agent --verbose"),
        "/usr/bin/openlogi-agent"
    );
    assert_eq!(
        unescape_systemd_exec("\"/home/a b/openlogi-agent\" --verbose"),
        "/home/a b/openlogi-agent"
    );
}

#[test]
fn same_executable_compares_missing_paths_literally() {
    assert!(same_executable(
        Path::new("/nonexistent/openlogi-agent"),
        Path::new("/nonexistent/openlogi-agent")
    ));
    assert!(!same_executable(
        Path::new("/nonexistent/a"),
        Path::new("/nonexistent/b")
    ));
}

/// `/usr/bin` is a symlink into `/usr` on merged-`/usr` systems, so the
/// comparison has to resolve both sides rather than match strings.
#[test]
fn same_executable_resolves_symlinks() {
    let dir = std::env::temp_dir().join(format!("openlogi-unit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let target = dir.join("openlogi-agent");
    std::fs::write(&target, b"#!/bin/sh\n").expect("write target");
    let link = dir.join("linked-agent");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");

    assert!(same_executable(&link, &target));
    std::fs::remove_dir_all(&dir).ok();
}

/// The packaged-unit probe must not match a unit describing some other
/// binary — that is exactly when a generated unit is still needed.
#[test]
fn packaged_probe_ignores_a_unit_for_another_binary() {
    let unit = render_unit("/usr/bin/some-other-agent");
    let parsed = exec_start_value(&unit).map(unescape_systemd_exec);
    assert_eq!(parsed.as_deref(), Some("/usr/bin/some-other-agent"));
    assert!(!same_executable(
        Path::new("/usr/bin/some-other-agent"),
        Path::new("/usr/bin/openlogi-agent")
    ));
}

/// A unit this app did not render is never overwritten or deleted, at either
/// user-writable tier. The same round-trip check gates the config-tier
/// migration, the data-tier delete, and the decision to leave a file alone.
#[test]
fn a_unit_this_app_did_not_render_is_not_ours() {
    let ours = render_unit("/usr/bin/openlogi-agent");
    assert!(
        is_generated_unit(&ours),
        "our own rendering is not the user's"
    );

    let theirs = ours.replace(
        "Restart=on-failure",
        "Environment=RUST_LOG=debug\nRestart=on-failure",
    );
    assert!(
        !is_generated_unit(&theirs),
        "an edited unit belongs to whoever wrote it"
    );
}

/// The marker is what distinguishes an enablement this app made from one the
/// documented `systemctl --user enable --now` step made. It lives beside the
/// config, not among the units, so it cannot collide with a unit file.
#[test]
fn the_enablement_marker_sits_outside_the_unit_directories() {
    let marker = enablement_marker_path().expect("marker path resolves");
    let generated = generated_unit_path().expect("generated path resolves");
    let legacy = legacy_unit_path().expect("legacy path resolves");

    assert_ne!(marker, generated);
    assert_ne!(marker, legacy);
    assert_ne!(
        marker.parent(),
        generated.parent(),
        "the marker must not share a directory with the generated unit"
    );
    assert!(!marker.to_string_lossy().contains("systemd/user"));
}
