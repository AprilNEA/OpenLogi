use super::*;

/// A bundle skeleton with an empty `Info.plist` per component — in *both*
/// channels' layouts, so a cross-channel [`verify`] reports the identity it
/// found rather than a missing file.
fn bundle() -> tempfile::TempDir {
    let app = tempfile::tempdir().unwrap();
    for channel in [Channel::Production, Channel::Dev] {
        for &component in Component::VARIANTS {
            let plist = component.info_plist(app.path(), channel);
            fs_err::create_dir_all(plist.parent().unwrap()).unwrap();
            plist::Value::Dictionary(plist::Dictionary::new())
                .to_file_xml(plist)
                .unwrap();
        }
    }
    app
}

/// `--channel`'s default is rendered through `Display` and then parsed back
/// by clap's value parser, so a name only one of the two knows would break
/// `macos bundle` the moment the flag is omitted.
#[test]
fn each_channel_renders_as_the_flag_value_it_parses_from() {
    for channel in [Channel::Production, Channel::Dev] {
        assert_eq!(
            Channel::from_str(&channel.to_string(), false).ok(),
            Some(channel),
            "{channel} does not round-trip through the value parser"
        );
    }
}

#[test]
fn a_dev_bundle_can_never_collide_with_a_shipped_one() {
    let shipped: Vec<Identity> = Component::VARIANTS
        .iter()
        .map(|&component| Channel::Production.identity(component))
        .collect();

    for &component in Component::VARIANTS {
        let dev = Channel::Dev.identity(component);
        assert!(
            shipped.iter().all(|other| other.bundle_id != dev.bundle_id),
            "dev {component} id {} collides with a shipped identity",
            dev.bundle_id
        );
        assert!(
            shipped.iter().all(|other| other.name != dev.name),
            "dev {component} name {} collides with a shipped identity",
            dev.name
        );
    }
}

#[test]
fn shipped_identities_are_distinct_per_component() {
    let ids: Vec<String> = Component::VARIANTS
        .iter()
        .map(|&component| Channel::Production.identity(component).bundle_id)
        .collect();
    for (index, id) in ids.iter().enumerate() {
        assert!(
            !ids[index + 1..].contains(id),
            "{id} is claimed by two components"
        );
    }
}

/// Two bundles from the two channels can sit side by side on one machine —
/// the dev app under `target/dev`, the installed one in `/Applications` —
/// and macOS distinguishes their helpers by directory name whenever the
/// bundle metadata is stale.
#[test]
fn the_channels_never_share_a_helper_directory() {
    for &component in Component::VARIANTS {
        match (
            component.nested_bundle(Channel::Production),
            component.nested_bundle(Channel::Dev),
        ) {
            // The app itself is not nested; its two channels are kept apart
            // by living in different build directories.
            (None, None) => {}
            (Some(shipped), Some(dev)) => assert_ne!(
                shipped, dev,
                "{component} would occupy the same directory on both channels"
            ),
            (shipped, dev) => {
                panic!("{component} is nested on one channel only: {shipped:?} vs {dev:?}")
            }
        }
    }
}

#[test]
fn stamping_a_channel_makes_it_verify() {
    for channel in [Channel::Production, Channel::Dev] {
        let app = bundle();

        stamp(app.path(), channel, Component::VARIANTS).unwrap();

        verify(app.path(), channel, Component::VARIANTS).unwrap();
    }
}

#[test]
fn a_dev_bundle_fails_production_verification() {
    let app = bundle();
    stamp(app.path(), Channel::Dev, Component::VARIANTS).unwrap();

    let error = verify(app.path(), Channel::Production, Component::VARIANTS)
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("org.openlogi.openlogi-dev") && error.contains("production"),
        "the error must name the dev identity it found and the channel it wanted, got: {error}"
    );
}

#[test]
fn a_shipped_bundle_fails_dev_verification() {
    let app = bundle();
    stamp(app.path(), Channel::Production, Component::VARIANTS).unwrap();

    let error = verify(app.path(), Channel::Dev, Component::VARIANTS)
        .unwrap_err()
        .to_string();

    assert!(error.contains("dev"), "got: {error}");
}

#[test]
fn verify_rejects_a_bundle_with_no_identity_at_all() {
    let app = bundle();

    assert!(verify(app.path(), Channel::Production, Component::VARIANTS).is_err());
}

#[test]
fn missing_icons_are_reported_per_component() {
    let app = bundle();
    stamp(app.path(), Channel::Production, Component::VARIANTS).unwrap();

    let error = verify_icons(app.path(), Channel::Production, Component::VARIANTS)
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("missing the shared app icon"),
        "got: {error}"
    );
}
