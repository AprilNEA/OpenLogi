//! Loading and saving the file: the canonical example, backups and rotation, a missing file, comment-preserving tracked saves, and what the TOML layout omits.

use super::*;

#[test]
fn canonical_configuration_example_parses() {
    let body = include_str!("../../../../../docs/config.example.toml");
    let config: Config = toml::from_str(body).expect("documented config must parse");
    assert_eq!(config.schema_version, SCHEMA_VERSION);
    let bindings = config.stored_bindings("receiver:aabbccdd:slot:1");
    let Some(Binding::LongPress(long_press)) = bindings.get(&ButtonId::DpiToggle) else {
        panic!("documented long-press binding should keep its shape");
    };
    assert_eq!(long_press.short(), &Action::ShowDesktop);
    assert_eq!(long_press.long(), &Action::MissionControl);
}

#[test]
fn first_save_preserves_the_previous_config_for_recovery() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let backup = dir.path().join("config.toml.backup.1");
    let original = b"schema_version = 3\nselected_device = \"original\"\n";
    fs::write(&path, original).expect("write original config");

    let mut config = Config {
        selected_device: Some("replacement".to_string()),
        ..Config::default()
    };
    config.save_to_path(&path).expect("save replacement");
    assert_eq!(fs::read(&backup).expect("read backup"), original);

    config.selected_device = Some("second-save".to_string());
    config.save_to_path(&path).expect("save again");
    assert_eq!(
        fs::read(&backup).expect("read original backup"),
        original,
        "later saves in one process must not replace the recovery copy"
    );
}

#[test]
fn migrated_load_backs_up_the_pre_migration_source_exactly_once() {
    // A key-rewriting migration touches every entry, so the pre-migration
    // file is the user's only recovery path if the rewrite is wrong. The
    // backup must hold the source exactly as loaded — not the migrated,
    // re-serialized output — and must not be retaken on a later save from
    // the same `ConfigFile` (`migrated_from` is consumed with `Option::take`).
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    // Appended to the full file name, not substituted for its extension:
    // `config.toml` + `.v4.bak`, never `config.v4.bak`.
    let backup = dir.path().join("config.toml.v4.bak");
    let original =
        b"schema_version = 4\n\n[devices.\"direct:046d:c08d:unit:6be9d300\"]\ninvert_scroll = true\n";
    fs::write(&path, original).expect("write v4 config");

    let (config, mut file) = ConfigFile::load_from_path(&path).expect("load v4 config");
    assert!(
        config.devices.contains_key("unit:6be9d300"),
        "sanity: the key migration ran"
    );

    file.save(&config).expect("save migrated config");
    assert_eq!(
        fs::read(&backup).expect("read migration backup"),
        original,
        "the backup preserves the pre-migration source, not the rewritten output"
    );

    // Replace the backup with a sentinel that `original` never equals, so a
    // second write is observable even though it would write the same bytes
    // as the first: re-asserting against `original` here cannot distinguish
    // "not rewritten" from "rewritten with identical content", which is
    // exactly the regression this test exists to catch (`migrated_from`
    // going from a consuming `Option::take` to a plain read).
    let sentinel = b"sentinel: a second save must not touch this file";
    fs::write(&backup, sentinel).expect("overwrite backup with sentinel");

    let mut second = config.clone();
    second.selected_device = Some("unit:6be9d300".to_string());
    file.save(&second)
        .expect("save again from the same ConfigFile");
    assert_eq!(
        fs::read(&backup).expect("read backup after second save"),
        sentinel,
        "a second save from the same ConfigFile must not rewrite the backup"
    );
}

#[test]
fn config_backups_rotate_between_generations() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, b"first").expect("write first generation");
    super::backup_existing_config(&path).expect("back up first generation");

    fs::write(&path, b"second").expect("write second generation");
    super::backup_existing_config(&path).expect("back up second generation");

    assert_eq!(
        fs::read(super::config_backup_path(&path, 1).expect("backup path"))
            .expect("read newest backup"),
        b"second"
    );
    assert_eq!(
        fs::read(super::config_backup_path(&path, 2).expect("backup path"))
            .expect("read older backup"),
        b"first"
    );
}

#[test]
fn missing_file_yields_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nonexistent.toml");
    let cfg = Config::load_from_path(&path).expect("load");
    assert_eq!(cfg.schema_version, SCHEMA_VERSION);
    assert!(cfg.devices.is_empty());
}

#[test]
fn human_readable_toml_layout() {
    let mut cfg = Config::default();
    cfg.set_binding(
        "2b042",
        ButtonId::Back,
        Binding::Single(Action::BrowserBack),
    );
    let body = toml::to_string_pretty(&cfg).expect("serialize");

    // The key only contains [A-Za-z0-9_], so TOML emits it as a bare-word
    // table key (no surrounding quotes). The test asserts the observable
    // structure rather than locking in a specific quoting.
    assert!(
        body.contains(&format!("schema_version = {SCHEMA_VERSION}")),
        "got: {body}"
    );
    assert!(body.contains("[devices.2b042.bindings]"), "got: {body}");
    // A `Single` binding serializes byte-identically to the pre-v2 bare
    // `Action`, so the leaf line is unchanged.
    assert!(body.contains("Back = \"BrowserBack\""), "got: {body}");
}

#[test]
fn selected_device_roundtrips() {
    let mut cfg = Config::default();
    assert_eq!(cfg.selected_device(), None);
    cfg.set_selected_device(Some("2b042".into()));
    let parsed = write_and_read(&cfg);
    assert_eq!(parsed.selected_device(), Some("2b042"));
}

#[test]
fn cleared_selected_device_omits_field() {
    let mut cfg = Config::default();
    cfg.set_selected_device(Some("2b042".into()));
    cfg.set_selected_device(None);
    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(
        !body.contains("selected_device"),
        "cleared selection should not appear: {body}"
    );
}

#[test]
fn empty_device_block_is_skipped_in_output() {
    // Inserting then clearing should not leave a [devices."x"] header
    // with no bindings under it (skip_serializing_if on bindings).
    let mut cfg = Config::default();
    cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Copy));
    cfg.devices
        .get_mut("2b042")
        .expect("entry")
        .bindings
        .clear();
    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(
        !body.contains("Back"),
        "cleared bindings should not appear: {body}"
    );
}

#[test]
fn tracked_save_preserves_comments_and_rejects_concurrent_edits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "# keep this comment\nschema_version = 6\nselected_device = \"one\" # and this one\n",
    )
    .expect("write");
    let (mut config, mut file) = ConfigFile::load_from_path(&path).expect("load tracked");
    config.set_selected_device(Some("two".into()));
    file.save(&config).expect("save tracked");
    let saved = fs::read_to_string(&path).expect("read saved");
    assert!(saved.contains("# keep this comment"));
    assert!(saved.contains("# and this one"));
    assert!(saved.contains("selected_device = \"two\""));

    let external = format!("{saved}# external editor\n");
    fs::write(&path, &external).expect("external edit");
    config.set_selected_device(Some("three".into()));
    assert_matches!(file.save(&config), Err(ConfigError::Conflict { .. }));
    assert_eq!(fs::read_to_string(path).expect("read conflict"), external);
}

#[test]
fn a_failed_backup_write_leaves_the_migration_backup_still_owed() {
    // The pre-migration file is the user's only recovery path from a
    // key-rewriting migration. If the backup write fails — full disk,
    // read-only directory — and the debt is cleared anyway, the next save
    // that *does* succeed overwrites the v4 file with migrated content and no copy
    // ever exists.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    // Appended to the full file name, not substituted for its extension:
    // `config.toml` + `.v4.bak`, never `config.v4.bak`.
    let backup = dir.path().join("config.toml.v4.bak");
    let original =
        b"schema_version = 4\n\n[devices.\"direct:046d:c08d:unit:6be9d300\"]\ninvert_scroll = true\n";
    fs::write(&path, original).expect("write v4 config");
    // A directory where the backup file belongs: the write fails, and
    // nothing about the config file itself is wrong.
    fs::create_dir(&backup).expect("occupy the backup path");

    let (config, mut file) = ConfigFile::load_from_path(&path).expect("load v4 config");
    let error = file.save(&config).expect_err("the backup write must fail");
    assert_matches!(error, ConfigError::Write { .. });
    assert_eq!(
        fs::read(&path).expect("read config"),
        original,
        "a failed save leaves the v4 file in place"
    );

    fs::remove_dir(&backup).expect("free the backup path");
    file.save(&config).expect("save once the path is writable");
    assert_eq!(
        fs::read(&backup).expect("read migration backup"),
        original,
        "the retried save still has the pre-migration source to write"
    );
}
