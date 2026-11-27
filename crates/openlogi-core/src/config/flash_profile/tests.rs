use super::*;

const KEY: &str = "receiver:C531:slot:1";

fn single(action: Action) -> Binding {
    Binding::Single(action)
}

/// A config whose entry edits page 2 (G6 = Copy) and parks page 3 (G6 = Paste).
fn two_profiles() -> Config {
    let mut config = Config::default();
    config.select_flash_profile(KEY, 3);
    config.set_binding(KEY, ButtonId::G6, single(Action::Paste));
    config.select_flash_profile(KEY, 2);
    config.set_binding(KEY, ButtonId::G6, single(Action::Copy));
    config
}

fn g6(config: &Config, page: u8) -> Option<Binding> {
    config
        .flash_profile_config(KEY, page)
        .devices
        .get(KEY)?
        .bindings
        .get(&ButtonId::G6)
        .cloned()
}

#[test]
fn each_page_reads_its_own_bindings() {
    let config = two_profiles();
    assert_eq!(g6(&config, 2), Some(single(Action::Copy)));
    assert_eq!(g6(&config, 3), Some(single(Action::Paste)));
    assert_eq!(g6(&config, 4), None, "a profile never configured has none");
    assert_eq!(config.flash_profile_pages(KEY), BTreeSet::from([2, 3]));
}

#[test]
fn selecting_a_profile_moves_no_binding_between_profiles() {
    let mut config = two_profiles();
    assert!(config.select_flash_profile(KEY, 3));
    let device = &config.devices[KEY];
    assert_eq!(device.flash_page, Some(3));
    assert_eq!(
        device.bindings.get(&ButtonId::G6),
        Some(&single(Action::Paste))
    );
    assert_eq!(g6(&config, 2), Some(single(Action::Copy)));
    assert!(!config.select_flash_profile(KEY, 3), "already selected");
    // Selecting an unconfigured profile parks the edited one and edits none.
    assert!(config.select_flash_profile(KEY, 8));
    assert!(config.devices[KEY].bindings.is_empty());
    assert_eq!(config.flash_profile_pages(KEY), BTreeSet::from([2, 3]));
}

#[test]
fn maps_held_for_no_page_become_the_first_selected_profiles() {
    let mut config = Config::default();
    config.set_binding(KEY, ButtonId::G7, single(Action::Copy));
    assert!(
        config.flash_profile_pages(KEY).is_empty(),
        "untagged maps apply nowhere"
    );
    assert_eq!(
        config.flash_profile_config(KEY, 2).devices[KEY]
            .bindings
            .get(&ButtonId::G7),
        None
    );
    config.select_flash_profile(KEY, 2);
    assert_eq!(config.flash_profile_pages(KEY), BTreeSet::from([2]));
}

#[test]
fn a_copy_takes_the_source_profiles_bindings() {
    let mut config = two_profiles();
    // From the edited profile.
    assert!(config.copy_flash_profile(KEY, 2, 8));
    assert_eq!(g6(&config, 8), Some(single(Action::Copy)));
    // From a parked one, over what the target held.
    assert!(config.copy_flash_profile(KEY, 3, 8));
    assert_eq!(g6(&config, 8), Some(single(Action::Paste)));
    assert!(!config.copy_flash_profile(KEY, 3, 8), "nothing new");
    assert_eq!(
        g6(&config, 2),
        Some(single(Action::Copy)),
        "the source is untouched"
    );
    assert_eq!(g6(&config, 3), Some(single(Action::Paste)));
}

#[test]
fn per_profile_bindings_round_trip_through_toml() {
    let mut config = two_profiles();
    config.set_per_app_binding(KEY, "org.mozilla.firefox", ButtonId::G6, Some(Action::Undo));
    let text = toml::to_string(&config.devices[KEY]).expect("serialize");
    let back: DeviceConfig = toml::from_str(&text).expect("parse");
    assert_eq!(back, config.devices[KEY]);
}
