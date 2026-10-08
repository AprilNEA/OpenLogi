//! Preview and restore through the same parser, backup policy, and atomic writer
//! as ordinary configuration saves. A preview pins both files until confirmation.

use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read as _},
    path::{Path, PathBuf},
};

use super::{
    CONFIG_BACKUP_GENERATIONS, ConfigError, config_backup_path, lock_config_writer,
    migration_backup_path, parse_config, render_config, write_atomic,
};
use crate::config::{Config, SCHEMA_VERSION};
use crate::file_input::FileInput;
use crate::optionsplus::ImportNotice;
#[cfg(target_os = "macos")]
use crate::optionsplus::{OptionsSettings, read_database};

#[derive(Debug)]
enum CandidateSource {
    Toml(String),
    #[cfg(target_os = "macos")]
    OptionsPlus {
        bytes: Vec<u8>,
        applications: std::collections::BTreeMap<PathBuf, Option<String>>,
    },
}

/// A changed setting in a recovery preview, after schema migration.
#[derive(Debug)]
pub struct ConfigChange {
    /// TOML key path, including the device or application identity where applicable.
    pub path: String,
    /// Individual keys, so consumers never have to split quoted TOML paths.
    pub keys: Vec<String>,
    /// Existing value; absent when the setting is being added.
    pub before: Option<toml::Value>,
    /// Restored value; absent when the setting is being removed.
    pub after: Option<toml::Value>,
}

/// A validated recovery candidate and the exact revisions the user previewed.
/// Preparing one never writes files. Applying one consumes it, so a retry needs
/// a fresh preview even when backup rotation or a write fails.
#[derive(Debug)]
pub struct RecoveryPlan {
    target: PathBuf,
    source: PathBuf,
    original: Option<Vec<u8>>,
    candidate: CandidateSource,
    body: String,
    changes: Vec<ConfigChange>,
    device_names: std::collections::BTreeMap<String, String>,
    current_error: Option<String>,
    notices: Vec<ImportNotice>,
    import_devices: Option<(String, String)>,
}

impl RecoveryPlan {
    /// Read a backup strictly: a missing source is an error, never a default
    /// configuration. An unreadable target is also an error; a malformed target
    /// can be recovered, with its original bytes preserved before replacement.
    pub fn prepare(target: &Path, source: &Path) -> Result<Self, ConfigError> {
        let candidate = read_candidate(source)?;
        let (config, _) = parse_config(source, &candidate)?;
        let original = read_optional(target)?;
        let (current, current_error) = match original.as_deref() {
            Some(bytes) => match std::str::from_utf8(bytes)
                .map_err(|error| error.to_string())
                .and_then(|text| {
                    parse_config(target, text)
                        .map(|(config, _)| config)
                        .map_err(|error| error.to_string())
                }) {
                Ok(config) => (Some(config), None),
                Err(error) => (None, Some(error)),
            },
            None => (None, None),
        };
        let mut changes = Vec::new();
        let before = current.as_ref().map(comparable_values).transpose()?;
        let after = comparable_values(&config)?;
        collect_changes(&[], before.as_ref(), Some(&after), &mut changes);
        let body = render_config(&config, Some(&candidate), source)?;
        Ok(Self {
            target: target.to_path_buf(),
            source: source.to_path_buf(),
            original,
            candidate: CandidateSource::Toml(candidate),
            body,
            changes,
            device_names: device_names(current.as_ref(), &config),
            current_error,
            notices: Vec::new(),
            import_devices: None,
        })
    }

    /// Read the existing import target under the recovery file policy, including
    /// regular-file, symlink and size checks. A missing target is never defaulted.
    #[cfg(target_os = "macos")]
    pub fn read_import_target(target: &Path) -> Result<Config, ConfigError> {
        read_import_target(target).map(|(_, config)| config)
    }

    /// Preview supported macOS Options+ assignments for an explicit device pair.
    /// Requires a valid existing target; malformed files must be recovered first.
    #[cfg(target_os = "macos")]
    pub fn prepare_options(
        target: &Path,
        source: &Path,
        source_device: &str,
        target_device: &str,
    ) -> Result<Self, ConfigError> {
        let bytes = read_database(source)?;
        let options = OptionsSettings::parse(&bytes)?;
        let (original, current) = read_import_target(target)?;
        let text = std::str::from_utf8(&original).map_err(|error| {
            read_error(target, io::Error::new(io::ErrorKind::InvalidData, error))
        })?;
        let applications = std::cell::RefCell::new(std::collections::BTreeMap::new());
        let merged = options.merge(&current, source_device, target_device, |path| {
            let path = Path::new(path);
            let identity = crate::app::bundle_identifier(path);
            applications
                .borrow_mut()
                .insert(path.to_path_buf(), identity.clone());
            identity
        })?;
        let mut changes = Vec::new();
        collect_changes(
            &[],
            Some(&comparable_values(&current)?),
            Some(&comparable_values(merged.config())?),
            &mut changes,
        );
        let body = render_config(merged.config(), Some(text), target)?;
        Ok(Self {
            target: target.to_path_buf(),
            source: source.to_path_buf(),
            original: Some(original),
            candidate: CandidateSource::OptionsPlus {
                bytes,
                applications: applications.into_inner(),
            },
            body,
            changes,
            device_names: device_names(Some(&current), merged.config()),
            current_error: None,
            notices: merged.notices().to_vec(),
            import_devices: Some((source_device.to_owned(), target_device.to_owned())),
        })
    }

    /// Whether this plan imports supported assignments rather than a full backup.
    #[must_use]
    pub fn is_options_import(&self) -> bool {
        match self.candidate {
            CandidateSource::Toml(_) => false,
            #[cfg(target_os = "macos")]
            CandidateSource::OptionsPlus { .. } => true,
        }
    }

    /// Explicit source and destination identities retained through confirmation.
    #[must_use]
    pub fn import_devices(&self) -> Option<(&str, &str)> {
        self.import_devices
            .as_ref()
            .map(|(source, target)| (source.as_str(), target.as_str()))
    }

    /// Unsupported assignments and behavior differences, never silently discarded.
    #[must_use]
    pub fn notices(&self) -> &[ImportNotice] {
        &self.notices
    }

    /// Settings that will differ from the current, migrated configuration.
    #[must_use]
    pub fn changes(&self) -> &[ConfigChange] {
        &self.changes
    }

    /// Device name from the candidate, or from the old file for removed devices.
    #[must_use]
    pub fn device_name<'a>(&'a self, key: &'a str) -> &'a str {
        self.device_names.get(key).map_or(key, String::as_str)
    }

    /// Why the current file cannot be compared, if it is damaged or unsupported.
    #[must_use]
    pub fn current_error(&self) -> Option<&str> {
        self.current_error.as_deref()
    }

    /// File the user selected, retained for the confirmation view.
    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Configuration file this preview may replace.
    #[must_use]
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// Replace the target only if both files still match the preview. Unlike
    /// routine saves, recovery preserves the current file outside the rotating
    /// backups. Even a failed restore must leave the selected source intact.
    pub fn apply(self) -> Result<(), ConfigError> {
        self.apply_with_source_check(Self::source_unchanged)
    }

    fn apply_with_source_check(
        self,
        mut source_unchanged: impl FnMut(&Self) -> Result<bool, ConfigError>,
    ) -> Result<(), ConfigError> {
        let _writer = lock_config_writer(&self.target)?;
        if !source_unchanged(&self)? {
            return Err(ConfigError::Conflict { path: self.source });
        }
        if read_optional(&self.target)? != self.original {
            return Err(ConfigError::Conflict { path: self.target });
        }
        if let Some(original) = &self.original {
            preserve_before_restore(&self.target, original).map_err(|source| {
                ConfigError::Write {
                    path: self.target.clone(),
                    source,
                }
            })?;
        }
        // Preserving a backup can take time on a slow filesystem. A revision that
        // arrives during that work must not be replaced by the earlier preview.
        if !source_unchanged(&self)? {
            return Err(ConfigError::Conflict { path: self.source });
        }
        if read_optional(&self.target)? != self.original {
            return Err(ConfigError::Conflict { path: self.target });
        }
        write_atomic(&self.target, self.body.as_bytes()).map_err(|source| ConfigError::Write {
            path: self.target,
            source,
        })
    }

    fn source_unchanged(&self) -> Result<bool, ConfigError> {
        match &self.candidate {
            CandidateSource::Toml(candidate) => Ok(read_candidate(&self.source)? == *candidate),
            #[cfg(target_os = "macos")]
            CandidateSource::OptionsPlus {
                bytes,
                applications,
            } => {
                if read_database(&self.source)? != *bytes {
                    return Ok(false);
                }
                Ok(applications
                    .iter()
                    .all(|(path, expected)| crate::app::bundle_identifier(path) == *expected))
            }
        }
    }
}

/// Recovery copies, newest first, then rotating backups and pre-migration files.
/// An inaccessible entry is reported rather than hidden.
pub fn recovery_backups(target: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    let paths = (1..=CONFIG_BACKUP_GENERATIONS)
        .map(|generation| config_backup_path(target, generation))
        .chain(
            (1..SCHEMA_VERSION)
                .rev()
                .map(|version| migration_backup_path(target, version)),
        );
    let mut backups: Vec<_> = before_restore_copies(target)
        .map_err(|error| read_error(target, error))?
        .into_iter()
        .map(|(_, path)| path)
        .collect();
    for path in paths {
        let path = path.map_err(|error| read_error(target, error))?;
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => backups.push(path),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(read_error(&path, error)),
        }
    }
    Ok(backups)
}

fn before_restore_prefix(target: &Path) -> io::Result<String> {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "config path has no UTF-8 file name",
            )
        })?;
    Ok(format!("{name}.before-restore."))
}

fn before_restore_copies(target: &Path) -> io::Result<Vec<(u64, PathBuf)>> {
    let prefix = before_restore_prefix(target)?;
    let entries = match fs::read_dir(target.parent().unwrap_or_else(|| Path::new("."))) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut copies = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(sequence) = name
            .to_str()
            .and_then(|name| name.strip_prefix(&prefix))
            .and_then(|suffix| suffix.strip_suffix(".bak"))
            .and_then(|sequence| sequence.parse::<u64>().ok())
        else {
            continue;
        };
        if entry.file_type()?.is_file() {
            copies.push((sequence, entry.path()));
        }
    }
    copies.sort_by_key(|(sequence, _)| std::cmp::Reverse(*sequence));
    Ok(copies)
}

/// Reserve a fresh name before using the shared atomic writer. The copy never
/// replaces an earlier recovery point or any rotating source backup.
fn preserve_before_restore(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let prefix = before_restore_prefix(target)?;
    let next = before_restore_copies(target)?
        .first()
        .map_or(Some(1), |(sequence, _)| sequence.checked_add(1))
        .ok_or_else(|| io::Error::other("no recovery backup name is available"))?;
    for sequence in next..=u64::MAX {
        let path = target.with_file_name(format!("{prefix}{sequence}.bak"));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(reservation) => {
                drop(reservation);
                if let Err(error) = write_atomic(&path, bytes) {
                    // Only our freshly reserved, empty file is removed.
                    let _ = fs::remove_file(&path);
                    return Err(error);
                }
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("no recovery backup name is available"))
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, ConfigError> {
    match read_recovery_file(path, FileInput::ReplacementTarget) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(read_error(path, error)),
    }
}

// Recovery accepts user-selected files and reopens them at confirmation. Keep
// every source/target read bounded, including files replaced after the preview.
const MAX_RECOVERY_BYTES: usize = 8 * 1024 * 1024;

fn read_candidate(path: &Path) -> Result<String, ConfigError> {
    let bytes =
        read_recovery_file(path, FileInput::Source).map_err(|error| read_error(path, error))?;
    String::from_utf8(bytes)
        .map_err(|error| read_error(path, io::Error::new(io::ErrorKind::InvalidData, error)))
}

fn read_recovery_file(path: &Path, role: FileInput) -> io::Result<Vec<u8>> {
    let file = role.open(path)?;
    let metadata = file.metadata()?;
    let too_large = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "configuration exceeds the 8 MiB recovery limit",
        )
    };
    if metadata.len() > MAX_RECOVERY_BYTES as u64 {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take((MAX_RECOVERY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_RECOVERY_BYTES {
        return Err(too_large());
    }
    Ok(bytes)
}

#[cfg(target_os = "macos")]
fn read_import_target(target: &Path) -> Result<(Vec<u8>, Config), ConfigError> {
    let original = read_recovery_file(target, FileInput::ReplacementTarget)
        .map_err(|error| read_error(target, error))?;
    let text = std::str::from_utf8(&original)
        .map_err(|error| read_error(target, io::Error::new(io::ErrorKind::InvalidData, error)))?;
    let (config, _) = parse_config(target, text)?;
    Ok((original, config))
}

fn read_error(path: &Path, source: io::Error) -> ConfigError {
    ConfigError::Read {
        path: path.to_path_buf(),
        source,
    }
}

fn comparable_values(config: &Config) -> Result<toml::Value, ConfigError> {
    let mut table = toml::Table::try_from(config)?;
    // Persistence omits the entire default AppSettings table. In a preview its
    // effective values still exist: changing one switch must not show every
    // other default preference as newly added or removed.
    table.insert(
        "app_settings".into(),
        toml::Value::try_from(&config.app_settings)?,
    );
    let mut devices = toml::Table::new();
    for (key, device) in &config.devices {
        let mut values = toml::Table::try_from(device)?;
        values.insert("enabled".into(), device.enabled.into());
        values.insert("invert_scroll".into(), device.invert_scroll.into());
        values.insert(
            "action_ring".into(),
            toml::Value::try_from(&device.action_ring)?,
        );
        if let Some(light) = &device.light {
            let mut light_values = toml::Table::try_from(light)?;
            light_values.insert("auto_camera".into(), light.auto_camera.into());
            values.insert("light".into(), light_values.into());
        }
        devices.insert(key.clone(), values.into());
    }
    table.insert("devices".into(), devices.into());
    Ok(toml::Value::Table(table))
}

fn device_names(
    before: Option<&Config>,
    after: &Config,
) -> std::collections::BTreeMap<String, String> {
    before
        .into_iter()
        .chain(std::iter::once(after))
        .flat_map(|config| &config.devices)
        .map(|(key, device)| {
            let name = device
                .custom_name
                .as_deref()
                .or_else(|| {
                    device
                        .identity
                        .as_ref()
                        .map(|identity| identity.display_name.as_str())
                })
                .unwrap_or(key);
            (key.clone(), name.to_owned())
        })
        .collect()
}

fn collect_changes(
    path: &[String],
    before: Option<&toml::Value>,
    after: Option<&toml::Value>,
    changes: &mut Vec<ConfigChange>,
) {
    if before == after {
        return;
    }
    let left = before.and_then(toml::Value::as_table);
    let right = after.and_then(toml::Value::as_table);
    // Keep payload actions intact: CustomShortcut is one assignment, not a
    // setting called "CustomShortcut" nested beneath a button.
    let is_action = path
        .iter()
        .any(|key| matches!(key.as_str(), "bindings" | "per_app_bindings"))
        && before
            .into_iter()
            .chain(after)
            .any(|value| value.clone().try_into::<crate::binding::Action>().is_ok());
    if !is_action && (left.is_some() || before.is_none()) && (right.is_some() || after.is_none()) {
        let keys: BTreeSet<_> = left
            .into_iter()
            .chain(right)
            .flat_map(|table| table.keys())
            .collect();
        for key in keys {
            let mut child = path.to_vec();
            child.push(key.clone());
            collect_changes(
                &child,
                left.and_then(|table| table.get(key)),
                right.and_then(|table| table.get(key)),
                changes,
            );
        }
    } else {
        let formatted = path
            .iter()
            .map(|key| {
                if key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                {
                    key.clone()
                } else {
                    format!("{key:?}")
                }
            })
            .collect::<Vec<_>>()
            .join(".");
        changes.push(ConfigChange {
            path: formatted,
            keys: path.to_vec(),
            before: before.cloned(),
            after: after.cloned(),
        });
    }
}

#[cfg(test)]
mod tests;
