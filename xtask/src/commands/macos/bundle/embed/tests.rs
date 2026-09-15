use strum::VariantArray as _;

use super::super::identity;
use super::*;
use crate::support::fs::repo_root;
use crate::support::info_plist::read_plist_string;

/// Identity work iterates every `Component`, so a component added without a
/// `Helper` to embed it would only surface as a stamping failure during a
/// real build.
#[test]
fn every_nested_component_is_embedded_by_a_helper() {
    for &component in Component::VARIANTS {
        assert!(
            component == Component::App
                || HELPERS.iter().any(|helper| helper.component == component),
            "{component} has no Helper entry to embed it"
        );
    }
}

fn touch_all(binaries: &[std::path::PathBuf]) {
    for path in binaries {
        fs_err::create_dir_all(path.parent().unwrap()).unwrap();
        fs_err::write(path, b"").unwrap();
    }
}

/// Both channels lay their helpers out differently, and both are assembled
/// by this code — `macos bundle --channel dev` and `macos dev-bundle`.
#[test]
fn verify_bundle_binaries_accepts_a_complete_bundle() {
    for channel in [Channel::Production, Channel::Dev] {
        let app = tempfile::tempdir().unwrap();
        touch_all(&required_bundle_binaries(app.path(), channel));

        verify_bundle_binaries(app.path(), channel).unwrap();
    }
}

#[test]
fn final_bundle_verification_rejects_a_missing_agent_launch_plist() {
    let channel = Channel::Production;
    let app = tempfile::tempdir().unwrap();
    touch_all(&required_bundle_binaries(app.path(), channel));

    let error = verify_bundle(app.path(), channel).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("missing required agent launch plist"),
        "unexpected error: {error}"
    );
}

#[test]
fn final_bundle_verification_accepts_a_matching_agent_launch_plist() {
    for channel in [Channel::Production, Channel::Dev] {
        let app = tempfile::tempdir().unwrap();
        touch_all(&required_bundle_binaries(app.path(), channel));
        write_agent_launch_plist(app.path(), channel).unwrap();

        verify_bundle(app.path(), channel).unwrap();
    }
}

#[test]
fn final_bundle_verification_rejects_a_plist_for_the_wrong_channel() {
    let channel = Channel::Production;
    let app = tempfile::tempdir().unwrap();
    touch_all(&required_bundle_binaries(app.path(), channel));
    let path = app
        .path()
        .join("Contents/Library/LaunchAgents")
        .join(format!("{}.plist", agent_service_label(channel)));
    fs_err::create_dir_all(path.parent().unwrap()).unwrap();
    plist::Value::Dictionary(agent_launch_plist(Channel::Dev).unwrap())
        .to_file_xml(&path)
        .unwrap();

    let error = verify_bundle(app.path(), channel).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("does not match the production bundle layout"),
        "unexpected error: {error}"
    );
}

/// The checked-in helper plists are what a fresh bundle starts from, so a
/// rename there that never reached the identity table would ship one name in
/// the bundle and another in every verification.
#[test]
fn shipped_helper_plists_declare_their_production_identity() {
    let root = repo_root().unwrap();

    for helper in &HELPERS {
        let plist = root.join(helper.info_plist);
        let expected = Channel::Production.identity(helper.component);

        for (key, want) in identity::identity_entries(&expected) {
            assert_eq!(
                read_plist_string(&plist, key).unwrap().as_deref(),
                Some(want),
                "{} declares the wrong {key}",
                helper.info_plist
            );
        }
    }
}

/// Every helper must declare the shared icon, or it shows up blank in the
/// System Settings panes where users grant it permissions.
#[test]
fn shipped_helper_plists_declare_the_shared_icon() {
    let root = repo_root().unwrap();

    for helper in &HELPERS {
        let icon = read_plist_string(&root.join(helper.info_plist), "CFBundleIconFile").unwrap();

        assert_eq!(
            icon.as_deref().map(|file| file.trim_end_matches(".icns")),
            Some("AppIcon"),
            "{} must declare the shared app icon",
            helper.info_plist
        );
    }
}

/// The launchd labels are frozen once shipped: `SMAppService` registrations
/// users approved in Login Items key on them, so a rename orphans every
/// existing registration. Pinned as literals on purpose — routing the
/// expectation through `brand::` would make a rename pass silently.
#[test]
fn agent_service_labels_are_frozen() {
    assert_eq!(
        agent_service_label(Channel::Production),
        "org.openlogi.agent.service"
    );
    assert_eq!(
        agent_service_label(Channel::Dev),
        "org.openlogi.agent.service-dev"
    );
}

/// The embedded service plist is what launchd starts the agent from: its
/// `BundleProgram` must name the channel's real helper layout, and its
/// `KeepAlive` must respawn crashes without resurrecting a deliberate Quit
/// (`SuccessfulExit: false`).
#[test]
fn agent_launch_plist_targets_the_channel_helper_and_keeps_alive() {
    for (channel, helper_dir) in [
        (Channel::Production, "OpenLogi Agent.app"),
        (Channel::Dev, "OpenLogi Agent Dev.app"),
    ] {
        let content = agent_launch_plist(channel).unwrap();

        assert_eq!(
            content.get("Label").and_then(plist::Value::as_string),
            Some(agent_service_label(channel).as_str())
        );
        assert_eq!(
            content
                .get("BundleProgram")
                .and_then(plist::Value::as_string),
            Some(
                format!("Contents/Library/LoginItems/{helper_dir}/Contents/MacOS/openlogi-agent")
                    .as_str()
            )
        );
        let keep_alive = content
            .get("KeepAlive")
            .and_then(plist::Value::as_dictionary)
            .unwrap();
        assert_eq!(
            keep_alive
                .get("SuccessfulExit")
                .and_then(plist::Value::as_boolean),
            Some(false)
        );
    }
}

/// `write_agent_launch_plist` must refuse a bundle whose helper is absent —
/// a registration pointing at nothing is a login item that silently never
/// starts.
#[test]
fn agent_launch_plist_refuses_a_bundle_without_the_helper() {
    let app = tempfile::tempdir().unwrap();

    let error = write_agent_launch_plist(app.path(), Channel::Dev).unwrap_err();

    assert!(
        error.to_string().contains("after the helpers are embedded"),
        "unexpected error: {error}"
    );
}

#[test]
fn verify_bundle_binaries_names_each_missing_binary() {
    let channel = Channel::Production;
    let count = required_bundle_binaries(Path::new("/probe"), channel).len();

    for skipped in 0..count {
        let app = tempfile::tempdir().unwrap();
        let required = required_bundle_binaries(app.path(), channel);
        let missing = required[skipped].clone();
        let shipped: Vec<_> = required
            .into_iter()
            .filter(|path| *path != missing)
            .collect();
        touch_all(&shipped);

        let error = verify_bundle_binaries(app.path(), channel).unwrap_err();

        assert!(
            error.to_string().ends_with(&missing.display().to_string()),
            "error should name {}, got: {error}",
            missing.display()
        );
    }
}
