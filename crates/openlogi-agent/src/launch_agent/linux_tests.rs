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

/// Scratch directory for tests that need real files. Removed on drop so a
/// failing assertion cannot leave state behind for the next run.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "openlogi-launch-agent-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A claim held from an earlier reconcile must survive a transient enable
/// failure — no session bus yet at login, say. Dropping it would leave an
/// enablement this app owns with nothing able to withdraw it, so `Launch at
/// login: off` would never turn autostart back off.
#[test]
fn a_held_claim_survives_a_failed_re_enable() {
    let dir = TempDir::new("held-claim");
    let marker = dir.path("autostart.enabled");
    std::fs::write(&marker, b"").expect("pre-existing claim");

    enable_unit_with(&marker, || false);

    assert!(
        marker.exists(),
        "a claim from an earlier reconcile must not be dropped by a failed enable"
    );
}

/// A claim created by this call is rolled back when the enable fails, so no
/// marker outlives an enablement that never happened.
#[test]
fn a_claim_made_now_is_rolled_back_when_enable_fails() {
    let dir = TempDir::new("fresh-claim");
    let marker = dir.path("autostart.enabled");

    enable_unit_with(&marker, || false);

    assert!(
        !marker.exists(),
        "a claim this call created must not outlive a failed enable"
    );
}

#[test]
fn a_claim_is_kept_when_enable_succeeds() {
    let dir = TempDir::new("ok-claim");
    let marker = dir.path("autostart.enabled");

    enable_unit_with(&marker, || true);
    assert!(marker.exists(), "a successful enable is recorded");

    // A second reconcile finds the claim already held and keeps it.
    enable_unit_with(&marker, || true);
    assert!(marker.exists());
}

#[test]
fn recording_reports_whether_the_claim_is_new() {
    let dir = TempDir::new("record");
    let marker = dir.path("nested/autostart.enabled");

    assert!(
        record_enablement_at(&marker).expect("first claim"),
        "the first call creates the claim"
    );
    assert!(
        !record_enablement_at(&marker).expect("second claim"),
        "a claim already held is not created again"
    );
}

/// systemd resolves a unit name to the highest-precedence file that exists, so
/// a lower entry must never be matched past a higher one naming a different
/// executable — enabling on the strength of a shadowed entry would start a
/// binary this app never verified.
#[test]
fn only_the_effective_system_unit_is_considered() {
    let dir = TempDir::new("precedence");
    let high = dir.path("high");
    let low = dir.path("low");
    std::fs::create_dir_all(&high).expect("high dir");
    std::fs::create_dir_all(&low).expect("low dir");

    let ours = std::env::current_exe().expect("current exe");
    std::fs::write(low.join(UNIT_NAME), render_unit(&ours.to_string_lossy())).expect("low unit");

    let dirs = [high.clone(), low.clone()];

    // Only the lower entry exists: it is the effective unit, and it matches.
    assert_eq!(
        packaged_unit_in(&dirs, &ours).as_deref(),
        Some(low.join(UNIT_NAME).as_path()),
        "the sole existing unit is the effective one"
    );

    // A higher-precedence unit for a different binary now shadows it.
    std::fs::write(
        high.join(UNIT_NAME),
        render_unit("/usr/bin/some-other-agent"),
    )
    .expect("high unit");
    assert_eq!(
        packaged_unit_in(&dirs, &ours),
        None,
        "a shadowed match must not be reported"
    );

    // The effective unit naming this binary is reported, shadowing or not.
    std::fs::write(high.join(UNIT_NAME), render_unit(&ours.to_string_lossy()))
        .expect("high unit rewritten");
    assert_eq!(
        packaged_unit_in(&dirs, &ours).as_deref(),
        Some(high.join(UNIT_NAME).as_path()),
        "the highest-precedence unit is the one reported"
    );
}

/// The load path is environment dependent, so it is read from systemd rather
/// than assumed. Parsing keeps precedence order and ignores blank padding.
#[test]
fn the_reported_load_path_is_parsed_in_order() {
    let listing = "\
/home/u/.config/systemd/user.control
/home/u/.config/systemd/user

/etc/systemd/user
   /usr/lib/systemd/user   
";
    assert_eq!(
        parse_unit_paths(listing),
        vec![
            PathBuf::from("/home/u/.config/systemd/user.control"),
            PathBuf::from("/home/u/.config/systemd/user"),
            PathBuf::from("/etc/systemd/user"),
            PathBuf::from("/usr/lib/systemd/user"),
        ]
    );
    assert!(parse_unit_paths("").is_empty());
}

/// The two tiers this app writes to are removed, so the probe only ever sees
/// directories a unit could arrive in from somewhere else. Order survives.
#[test]
fn our_own_tiers_are_excluded_from_the_load_path() {
    let dirs = vec![
        PathBuf::from("/home/u/.config/systemd/user"),
        PathBuf::from("/etc/systemd/user"),
        PathBuf::from("/home/u/.local/share/systemd/user"),
        PathBuf::from("/home/u/.local/share/flatpak/exports/share/systemd/user"),
        PathBuf::from("/usr/lib/systemd/user"),
    ];
    let ours = [
        PathBuf::from("/home/u/.config/systemd/user"),
        PathBuf::from("/home/u/.local/share/systemd/user"),
    ];

    assert_eq!(
        exclude_dirs(dirs, &ours),
        vec![
            PathBuf::from("/etc/systemd/user"),
            PathBuf::from("/home/u/.local/share/flatpak/exports/share/systemd/user"),
            PathBuf::from("/usr/lib/systemd/user"),
        ],
        "a tier this app writes to is never treated as somebody else's"
    );
}

/// The resolved path must keep the tiers the old static list omitted — the
/// runtime directory, the session's exports — and must not contain ours.
#[test]
fn the_resolved_load_path_excludes_our_tiers() {
    let dirs = user_unit_dirs();
    assert!(!dirs.is_empty(), "the fallback alone is non-empty");

    let generated = generated_unit_path().expect("generated path");
    let legacy = legacy_unit_path().expect("legacy path");
    for ours in [generated.parent(), legacy.parent()].into_iter().flatten() {
        assert!(
            !dirs.iter().any(|dir| dir == ours),
            "{} must not be probed as a packaged tier",
            ours.display()
        );
    }
}

/// The fallback stands in when systemd cannot be asked, so it has to mirror the
/// documented load path — including the tiers a fixed list of absolute paths
/// misses. The runtime tier matters most: a unit there outranks the one this
/// app generates.
#[test]
fn the_fallback_covers_the_xdg_derived_tiers() {
    let dirs = fallback_unit_dirs();
    let has = |needle: &str| dirs.iter().any(|dir| dir == Path::new(needle));

    assert!(has("/etc/systemd/user"), "{dirs:?}");
    assert!(has("/run/systemd/user"), "{dirs:?}");
    assert!(has("/usr/lib/systemd/user"), "{dirs:?}");
    assert!(has("/usr/local/lib/systemd/user"), "{dirs:?}");

    // $XDG_DATA_DIRS defaults, appended with systemd/user.
    assert!(
        has("/usr/share/systemd/user") || std::env::var_os("XDG_DATA_DIRS").is_some(),
        "the data-dirs default must be expanded: {dirs:?}"
    );

    // The runtime tier is present whenever the session exports one.
    if std::env::var_os("XDG_RUNTIME_DIR").is_some() {
        assert!(
            dirs.iter()
                .any(|dir| dir.ends_with("systemd/user") && dir.starts_with("/run")),
            "the runtime tier outranks the generated unit and must be probed: {dirs:?}"
        );
    }
}

/// Colon-separated XDG base lists expand to one unit directory each, in order,
/// and relative entries are ignored the way systemd ignores them.
#[test]
fn xdg_base_lists_expand_in_order() {
    let mut dirs = Vec::new();
    push_xdg_dirs(&mut dirs, "OPENLOGI_TEST_UNSET_VAR", "/one:/two");
    assert_eq!(
        dirs,
        vec![
            PathBuf::from("/one/systemd/user"),
            PathBuf::from("/two/systemd/user"),
        ],
        "an unset variable falls back to the default list"
    );

    let mut dirs = Vec::new();
    push_xdg_dirs(&mut dirs, "PATH", "/unused");
    assert!(
        dirs.iter()
            .all(|dir| dir.is_absolute() && dir.ends_with("systemd/user")),
        "every expanded entry is an absolute unit directory: {dirs:?}"
    );
}
