use super::*;
use crate::config::{Config, ConfigFile};
use crate::hid::Dpi;

#[test]
#[cfg(unix)]
fn recovery_rejects_fifo_sources_and_targets_without_blocking() {
    use std::{sync::mpsc, thread, time::Duration};

    for (replace_source, confirming) in [(true, false), (false, false), (true, true), (false, true)]
    {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        let source = dir.path().join("source.toml");
        fs::write(&target, config_text(800)).unwrap();
        fs::write(&source, config_text(1600)).unwrap();
        let plan = confirming.then(|| RecoveryPlan::prepare(&target, &source).unwrap());
        let fifo = if replace_source { &source } else { &target }.clone();
        fs::remove_file(&fifo).unwrap();
        crate::file_input::tests::create_fifo(&fifo);
        let (tx, rx) = mpsc::channel();
        let read_target = target.clone();
        let read_source = source.clone();
        let reader = thread::spawn(move || {
            let result = match plan {
                Some(plan) => plan.apply(),
                None => RecoveryPlan::prepare(&read_target, &read_source).map(drop),
            };
            tx.send(result).unwrap();
        });
        let answer = rx.recv_timeout(Duration::from_secs(1));
        if matches!(answer, Err(mpsc::RecvTimeoutError::Timeout)) {
            // Release a regressed blocking reader before failing the test.
            drop(
                fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&fifo)
                    .unwrap(),
            );
        }
        reader.join().unwrap();
        let error = answer
            .expect("recovery read blocked on a FIFO")
            .unwrap_err();
        assert!(matches!(error, ConfigError::Read { path, .. } if path == fifo));
        fs::remove_file(&fifo).unwrap();
        fs::write(&fifo, config_text(if replace_source { 1600 } else { 800 })).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), config_text(800));
        assert_eq!(fs::read_to_string(&source).unwrap(), config_text(1600));
        assert!(recovery_backups(&target).unwrap().is_empty());
        let (config, mut file) = ConfigFile::load_from_path(&target).unwrap();
        file.save(&config)
            .expect("a rejected FIFO released the writer lock");
    }
}

#[test]
#[cfg(unix)]
fn recovery_preserves_symlink_targets_and_accepts_regular_source_symlinks() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let source = dir.path().join("source.toml");
    let referent = dir.path().join("managed-config.toml");
    fs::write(&referent, config_text(800)).unwrap();
    fs::write(&source, config_text(1600)).unwrap();
    symlink(&referent, &target).unwrap();
    assert!(matches!(
        RecoveryPlan::prepare(&target, &source),
        Err(ConfigError::Read { path, .. }) if path == target
    ));
    assert_eq!(fs::read_link(&target).unwrap(), referent);
    assert_eq!(fs::read_to_string(&referent).unwrap(), config_text(800));

    fs::remove_file(&target).unwrap();
    fs::write(&target, config_text(800)).unwrap();
    let plan = RecoveryPlan::prepare(&target, &source).unwrap();
    fs::remove_file(&target).unwrap();
    symlink(&referent, &target).unwrap();
    assert!(matches!(
        plan.apply(),
        Err(ConfigError::Read { path, .. }) if path == target
    ));
    assert_eq!(fs::read_link(&target).unwrap(), referent);
    assert_eq!(fs::read_to_string(&referent).unwrap(), config_text(800));

    fs::remove_file(&target).unwrap();
    fs::write(&target, config_text(800)).unwrap();
    let source_link = dir.path().join("linked-backup.toml");
    symlink(&source, &source_link).unwrap();
    RecoveryPlan::prepare(&target, &source_link)
        .unwrap()
        .apply()
        .unwrap();
    assert_eq!(fs::read_link(&source_link).unwrap(), source);
    assert_eq!(fs::read_to_string(&source).unwrap(), config_text(1600));
    assert_eq!(
        Config::load_from_path(&target).unwrap().devices["test"].dpi,
        Some(Dpi::new(1600))
    );
}

#[test]
fn recovery_rejects_oversized_sources_and_targets_before_reading_them() {
    for (replace_source, confirming) in [(true, false), (false, false), (true, true), (false, true)]
    {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        let source = dir.path().join("source.toml");
        fs::write(&target, config_text(800)).unwrap();
        fs::write(&source, config_text(1600)).unwrap();
        let plan = confirming.then(|| RecoveryPlan::prepare(&target, &source).unwrap());
        let oversized = if replace_source { &source } else { &target };
        fs::OpenOptions::new()
            .write(true)
            .open(oversized)
            .unwrap()
            .set_len(8 * 1024 * 1024 + 1)
            .unwrap();
        let error = match plan {
            Some(plan) => plan.apply().unwrap_err(),
            None => RecoveryPlan::prepare(&target, &source).unwrap_err(),
        };
        assert!(matches!(error, ConfigError::Read { path, source }
            if path == *oversized && source.kind() == io::ErrorKind::InvalidData));
        assert_eq!(fs::metadata(oversized).unwrap().len(), 8 * 1024 * 1024 + 1);
        assert!(recovery_backups(&target).unwrap().is_empty());
    }
}

#[test]
fn preview_compares_default_preferences_as_effective_values() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let source = dir.path().join("source.toml");
    Config::default().save_to_path(&target).unwrap();
    let mut candidate = Config::default();
    candidate.app_settings.launch_at_login = false;
    candidate.save_to_path(&source).unwrap();
    let plan = RecoveryPlan::prepare(&target, &source).unwrap();
    assert_eq!(plan.changes().len(), 1);
    let change = &plan.changes()[0];
    assert_eq!(change.path, "app_settings.launch_at_login");
    assert_eq!(
        change.before.as_ref().map(ToString::to_string).as_deref(),
        Some("true")
    );
    assert_eq!(
        change.after.as_ref().map(ToString::to_string).as_deref(),
        Some("false")
    );
}

#[test]
fn preview_keeps_default_ring_slots_when_only_haptics_change() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let source = dir.path().join("source.toml");
    let mut current = Config::default();
    current
        .devices
        .entry("test".into())
        .or_default()
        .action_ring
        .haptics = false;
    current.save_to_path(&target).unwrap();
    current.devices.get_mut("test").unwrap().action_ring.haptics = true;
    current.save_to_path(&source).unwrap();
    let plan = RecoveryPlan::prepare(&target, &source).unwrap();
    assert_eq!(plan.changes().len(), 1);
    let change = &plan.changes()[0];
    assert_eq!(change.path, "devices.test.action_ring.haptics");
    assert_eq!(
        change.before.as_ref().map(ToString::to_string).as_deref(),
        Some("false")
    );
    assert_eq!(
        change.after.as_ref().map(ToString::to_string).as_deref(),
        Some("true")
    );
}

fn config_text(dpi: u16) -> String {
    format!("# Keep this comment\nschema_version = {SCHEMA_VERSION}\n[devices.test]\ndpi = {dpi}\n")
}

#[test]
fn preview_is_read_only_and_restore_preserves_current_even_after_an_ordinary_save() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let backup = config_backup_path(&target, 1).unwrap();
    fs::write(&target, config_text(800)).unwrap();
    let (mut current, mut file) = ConfigFile::load_from_path(&target).unwrap();
    current.devices.get_mut("test").unwrap().dpi = Some(Dpi::new(1600));
    file.save(&current).unwrap();
    let original = fs::read(&target).unwrap();
    let plan = RecoveryPlan::prepare(&target, &backup).unwrap();
    assert_eq!(fs::read(&target).unwrap(), original);
    let change = plan
        .changes()
        .iter()
        .find(|change| change.path == "devices.test.dpi")
        .unwrap();
    assert_eq!(
        change.before.as_ref().map(ToString::to_string).as_deref(),
        Some("1600")
    );
    assert_eq!(
        change.after.as_ref().map(ToString::to_string).as_deref(),
        Some("800")
    );
    plan.apply().unwrap();
    assert_eq!(
        Config::load_from_path(&target).unwrap().devices["test"].dpi,
        Some(Dpi::new(800))
    );
    assert_eq!(
        fs::read(recovery_backups(&target).unwrap().first().unwrap()).unwrap(),
        original
    );
    assert_eq!(fs::read_to_string(&backup).unwrap(), config_text(800));
    assert!(
        fs::read_to_string(&target)
            .unwrap()
            .contains("# Keep this comment")
    );
}

#[test]
fn missing_invalid_and_future_sources_never_replace_the_current_config() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let source = dir.path().join("source.toml");
    fs::write(&target, config_text(800)).unwrap();
    assert!(matches!(
        RecoveryPlan::prepare(&target, &source),
        Err(ConfigError::Read { .. })
    ));
    for invalid in [
        "not a config".to_owned(),
        format!("schema_version = {}", SCHEMA_VERSION + 1),
    ] {
        fs::write(&source, invalid).unwrap();
        RecoveryPlan::prepare(&target, &source).unwrap_err();
    }
    assert_eq!(fs::read_to_string(&target).unwrap(), config_text(800));
    assert!(recovery_backups(&target).unwrap().is_empty());
}

#[test]
fn changing_either_file_invalidates_confirmation() {
    for change_source in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        let source = dir.path().join("source.toml");
        fs::write(&target, config_text(800)).unwrap();
        fs::write(&source, config_text(1600)).unwrap();
        let plan = RecoveryPlan::prepare(&target, &source).unwrap();
        let changed = if change_source { &source } else { &target };
        fs::write(changed, config_text(2400)).unwrap();
        assert!(matches!(plan.apply(), Err(ConfigError::Conflict { path }) if path == *changed));
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            config_text(if change_source { 800 } else { 2400 })
        );
        assert!(recovery_backups(&target).unwrap().is_empty());
    }
}

#[test]
fn corrupt_target_is_backed_up_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let source = dir.path().join("source.toml");
    fs::write(&target, b"broken\xff").unwrap();
    fs::write(&source, config_text(1600)).unwrap();
    let plan = RecoveryPlan::prepare(&target, &source).unwrap();
    assert!(plan.current_error().is_some());
    plan.apply().unwrap();
    assert_eq!(
        fs::read(recovery_backups(&target).unwrap().first().unwrap()).unwrap(),
        b"broken\xff"
    );
    assert_eq!(
        Config::load_from_path(&target).unwrap().devices["test"].dpi,
        Some(Dpi::new(1600))
    );
}

#[test]
fn recovery_never_rotates_away_its_source_even_with_a_broken_backup_slot() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let source = config_backup_path(&target, CONFIG_BACKUP_GENERATIONS).unwrap();
    fs::write(&target, config_text(800)).unwrap();
    fs::write(&source, config_text(1600)).unwrap();
    let plan = RecoveryPlan::prepare(&target, &source).unwrap();
    fs::write(config_backup_path(&target, 4).unwrap(), config_text(2400)).unwrap();
    fs::create_dir(config_backup_path(&target, 3).unwrap()).unwrap();
    plan.apply().unwrap();
    assert_eq!(fs::read_to_string(&source).unwrap(), config_text(1600));
    assert_eq!(
        fs::read(recovery_backups(&target).unwrap().first().unwrap()).unwrap(),
        config_text(800).as_bytes()
    );
}

#[test]
fn missing_target_can_be_restored_and_old_backups_are_migrated() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("nested/config.toml");
    let source = dir.path().join("source.toml");
    fs::write(
        &source,
        config_text(1600).replace(
            &format!("schema_version = {SCHEMA_VERSION}"),
            "schema_version = 6",
        ),
    )
    .unwrap();
    let plan = RecoveryPlan::prepare(&target, &source).unwrap();
    assert!(plan.current_error().is_none());
    plan.apply().unwrap();
    let config = Config::load_from_path(&target).unwrap();
    assert_eq!(config.schema_version, SCHEMA_VERSION);
    assert_eq!(config.devices["test"].dpi, Some(Dpi::new(1600)));
    assert!(recovery_backups(&target).unwrap().is_empty());
}

#[test]
fn a_recovery_copy_can_undo_a_restore_without_overwriting_earlier_copies() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let source = dir.path().join("source.toml");
    fs::write(&target, config_text(800)).unwrap();
    fs::write(&source, config_text(1600)).unwrap();
    RecoveryPlan::prepare(&target, &source)
        .unwrap()
        .apply()
        .unwrap();
    let first = recovery_backups(&target).unwrap().remove(0);
    RecoveryPlan::prepare(&target, &first)
        .unwrap()
        .apply()
        .unwrap();
    assert_eq!(
        Config::load_from_path(&target).unwrap().devices["test"].dpi,
        Some(Dpi::new(800))
    );
    let copies = recovery_backups(&target).unwrap();
    assert_eq!(copies.len(), 2);
    assert_eq!(
        Config::load_from_path(&copies[0]).unwrap().devices["test"].dpi,
        Some(Dpi::new(1600))
    );
    assert_eq!(fs::read_to_string(&first).unwrap(), config_text(800));
}

#[test]
#[cfg(unix)]
fn backup_creation_failure_leaves_both_files_intact() {
    let dir = tempfile::tempdir().unwrap();
    // The target fits the filesystem's component limit, but its backup name
    // does not. This exercises a real I/O failure without chmod/root assumptions.
    let target = dir.path().join(format!("{}.toml", "x".repeat(240)));
    let source = dir.path().join("source.toml");
    fs::write(&target, config_text(800)).unwrap();
    fs::write(&source, config_text(1600)).unwrap();
    let plan = RecoveryPlan::prepare(&target, &source).unwrap();
    assert!(matches!(plan.apply(), Err(ConfigError::Write { .. })));
    assert_eq!(fs::read_to_string(&target).unwrap(), config_text(800));
    assert_eq!(fs::read_to_string(&source).unwrap(), config_text(1600));
}

#[test]
fn recovery_respects_the_config_writer_lock_and_releases_it_afterwards() {
    for original in [None, Some(config_text(800)), Some("broken config".into())] {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        let source = dir.path().join("source.toml");
        if let Some(original) = &original {
            fs::write(&target, original).unwrap();
        }
        fs::write(&source, config_text(1600)).unwrap();
        let plan = RecoveryPlan::prepare(&target, &source).unwrap();
        let lock_path = target.with_added_extension("lock");
        let writer = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        writer.try_lock().unwrap();

        let error = plan
            .apply()
            .expect_err("recovery must not bypass an active writer");
        assert!(matches!(
            error,
            ConfigError::Write { path, source }
                if path == lock_path && source.kind() == io::ErrorKind::WouldBlock
        ));
        assert_eq!(
            read_optional(&target).unwrap(),
            original.as_ref().map(|text| text.as_bytes().to_vec())
        );
        assert_eq!(fs::read_to_string(&source).unwrap(), config_text(1600));
        assert!(recovery_backups(&target).unwrap().is_empty());

        drop(writer);
        RecoveryPlan::prepare(&target, &source)
            .unwrap()
            .apply()
            .unwrap();
        let (mut config, mut file) = ConfigFile::load_from_path(&target).unwrap();
        config.devices.get_mut("test").unwrap().dpi = Some(Dpi::new(2400));
        file.save(&config)
            .expect("recovery released the shared writer lock");
        assert_eq!(
            Config::load_from_path(&target).unwrap().devices["test"].dpi,
            Some(Dpi::new(2400))
        );
    }
}

#[test]
fn recovery_rechecks_the_source_after_preserving_the_current_file() {
    for remove_source in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        let source = dir.path().join("source.toml");
        fs::write(&target, config_text(800)).unwrap();
        fs::write(&source, config_text(1600)).unwrap();
        let plan = RecoveryPlan::prepare(&target, &source).unwrap();
        let mut checks = 0;
        let error = plan
            .apply_with_source_check(|plan| {
                checks += 1;
                if checks == 2 {
                    if remove_source {
                        fs::remove_file(&source).unwrap();
                    } else {
                        fs::write(&source, config_text(2400)).unwrap();
                    }
                }
                plan.source_unchanged()
            })
            .unwrap_err();
        assert_eq!(checks, 2);
        if remove_source {
            assert!(matches!(error, ConfigError::Read { path, source: error }
                if path == source && error.kind() == io::ErrorKind::NotFound));
            assert!(!source.exists());
        } else {
            assert!(matches!(error, ConfigError::Conflict { path } if path == source));
            assert_eq!(fs::read_to_string(&source).unwrap(), config_text(2400));
        }
        assert_eq!(fs::read_to_string(&target).unwrap(), config_text(800));
        assert_eq!(
            fs::read_to_string(&recovery_backups(&target).unwrap()[0]).unwrap(),
            config_text(800)
        );
        let (config, mut file) = ConfigFile::load_from_path(&target).unwrap();
        file.save(&config)
            .expect("a failed restore released the writer lock");
    }
}

#[test]
fn recovery_keeps_the_writer_lock_through_both_revision_checks() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let source = dir.path().join("source.toml");
    fs::write(&target, config_text(800)).unwrap();
    fs::write(&source, config_text(1600)).unwrap();
    let plan = RecoveryPlan::prepare(&target, &source).unwrap();
    let mut checks = 0;
    plan.apply_with_source_check(|plan| {
        checks += 1;
        let (config, mut file) = ConfigFile::load_from_path(&target).unwrap();
        assert!(
            matches!(file.save(&config), Err(ConfigError::Write { source, .. })
            if source.kind() == io::ErrorKind::WouldBlock)
        );
        plan.source_unchanged()
    })
    .unwrap();
    assert_eq!(checks, 2);
    assert_eq!(
        Config::load_from_path(&target).unwrap().devices["test"].dpi,
        Some(Dpi::new(1600))
    );
}

#[test]
fn target_changed_during_final_source_check_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let source = dir.path().join("source.toml");
    fs::write(&target, config_text(800)).unwrap();
    fs::write(&source, config_text(1600)).unwrap();
    let plan = RecoveryPlan::prepare(&target, &source).unwrap();
    let mut checks = 0;
    let error = plan
        .apply_with_source_check(|plan| {
            checks += 1;
            if checks == 2 {
                fs::write(&target, config_text(2400)).unwrap();
            }
            plan.source_unchanged()
        })
        .unwrap_err();
    assert!(matches!(error, ConfigError::Conflict { .. }));
    assert_eq!(fs::read_to_string(&target).unwrap(), config_text(2400));
}
