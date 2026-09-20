//! Version-aware configuration loading and conflict-safe persistence.

use std::{
    collections::HashSet,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, PoisonError},
};

use atomic_write_file::AtomicWriteFile;
use serde::Deserialize;
use thiserror::Error;
use toml_edit::{DocumentMut, Item, Table};

use super::{Config, SCHEMA_VERSION};
use crate::paths::{self, PathsError};

const CONFIG_BACKUP_GENERATIONS: usize = 5;
static BACKED_UP_CONFIGS: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Failure loading or persisting `config.toml`.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The platform config directory could not be resolved.
    #[error("could not resolve config path: {0}")]
    Path(#[from] PathsError),
    /// Reading the config file from disk failed.
    #[error("could not read config at {path}: {source}")]
    Read {
        /// The config file the read targeted.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The file is not valid TOML for its declared schema.
    #[error("could not parse config at {path}: {source}")]
    Parse {
        /// The config file that failed to parse.
        path: PathBuf,
        /// The underlying TOML deserialization error, including line/column.
        #[source]
        source: Box<toml::de::Error>,
    },
    /// A field from an older schema was used with a version where it is invalid.
    #[error("config at {path} uses obsolete field {field} with schema_version {version}")]
    ObsoleteField {
        /// The config file containing the field.
        path: PathBuf,
        /// Dotted path to the obsolete field.
        field: String,
        /// Schema version declared by the file.
        version: u32,
    },
    /// The file changed after it was loaded, so overwriting it would lose edits.
    #[error("config at {path} changed on disk; restart OpenLogi to reload it")]
    Conflict {
        /// The concurrently modified config file.
        path: PathBuf,
    },
    /// Writing the updated config back to disk failed.
    #[error("could not write config at {path}: {source}")]
    Write {
        /// The config file the write targeted.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The in-memory config could not be serialized to TOML.
    #[error("could not serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),
    /// A generated or previously parsed TOML document could not be edited.
    #[error("could not preserve config formatting at {path}: {source}")]
    Edit {
        /// The config file whose document was being updated.
        path: PathBuf,
        /// The TOML editing parser error.
        #[source]
        source: Box<toml_edit::TomlError>,
    },
    /// The file declares a schema outside the supported version range.
    #[error("config at {path} has unsupported schema_version {found}")]
    UnsupportedSchemaVersion {
        /// The config file carrying the unsupported version.
        path: PathBuf,
        /// The schema version the file declared.
        found: u32,
    },
}

/// A loaded config file plus the exact source revision it came from.
///
/// Saving compares that source with the current file before writing, so an
/// editor or another process cannot be overwritten by a stale GUI snapshot.
/// Existing comments and formatting are retained for keys that still exist.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    path: PathBuf,
    source: Option<String>,
    /// Text of the file as loaded, when it was written by an older schema.
    /// Copied aside on the first save so a key-rewriting migration is
    /// recoverable; cleared once that save lands, so only the first
    /// *successful* one pays for it and a failed save still owes the copy.
    migrated_from: Option<(u32, String)>,
}

#[derive(Deserialize)]
struct ConfigHeader {
    schema_version: u32,
}

impl ConfigFile {
    /// Load the default user config, returning a writable default when the
    /// file does not exist yet.
    pub fn load_or_default() -> Result<(Config, Self), ConfigError> {
        Self::load_from_path(&paths::config_path()?)
    }

    /// Load `path`, retaining its source revision for conflict-safe saves.
    pub fn load_from_path(path: &Path) -> Result<(Config, Self), ConfigError> {
        match fs::read_to_string(path) {
            Ok(source) => {
                let (config, loaded_version) = parse_config(path, &source)?;
                let migrated_from =
                    (loaded_version < SCHEMA_VERSION).then(|| (loaded_version, source.clone()));
                Ok((
                    config,
                    Self {
                        path: path.to_path_buf(),
                        source: Some(source),
                        migrated_from,
                    },
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok((
                Config::default(),
                Self {
                    path: path.to_path_buf(),
                    source: None,
                    migrated_from: None,
                },
            )),
            Err(source) => Err(ConfigError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Save `config` only if the file still matches the loaded revision.
    pub fn save(&mut self, config: &Config) -> Result<(), ConfigError> {
        let current = match fs::read_to_string(&self.path) {
            Ok(source) => Some(source),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(ConfigError::Read {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        if current != self.source {
            return Err(ConfigError::Conflict {
                path: self.path.clone(),
            });
        }

        // Borrowed, not taken: a failed write must leave the recovery copy
        // still owed. Taking here and then failing — a full disk, a
        // read-only directory — would spend the only chance to keep the
        // pre-migration file, and the next save that *did* succeed would
        // overwrite it with migrated content and no backup anywhere.
        if let Some((version, original)) = self.migrated_from.as_ref() {
            let backup = migration_backup_path(&self.path, *version).map_err(|source| {
                ConfigError::Write {
                    path: self.path.clone(),
                    source,
                }
            })?;
            // Atomic like the config write itself: this is the only copy of
            // the pre-migration file, so an interrupted write must not be
            // able to leave a truncated one behind.
            write_atomic(&backup, original.as_bytes()).map_err(|source| ConfigError::Write {
                path: backup,
                source,
            })?;
        }

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: self.path.clone(),
                source,
            })?;
        }
        let body = render_config(config, self.source.as_deref(), &self.path)?;
        backup_config_once(&self.path).map_err(|source| ConfigError::Write {
            path: self.path.clone(),
            source,
        })?;
        write_atomic(&self.path, body.as_bytes()).map_err(|source| ConfigError::Write {
            path: self.path.clone(),
            source,
        })?;
        // The migrated file is gone from disk now, and its copy is safely
        // beside it — only here is the debt actually settled, so only here is
        // it cleared. A second save from this `ConfigFile` must not rewrite
        // the backup with content that is no longer pre-migration.
        self.migrated_from = None;
        self.source = Some(body);
        Ok(())
    }
}

impl Config {
    /// Loads the config from the default user path, returning a default when
    /// the file does not exist yet.
    pub fn load_or_default() -> Result<Self, ConfigError> {
        ConfigFile::load_or_default().map(|(config, _)| config)
    }

    /// Load from `path` without retaining a writable source revision.
    pub fn load_from_path(path: &Path) -> Result<Self, ConfigError> {
        ConfigFile::load_from_path(path).map(|(config, _)| config)
    }

    /// Atomically save to the default path. Long-lived writers should retain
    /// and use [`ConfigFile`] so concurrent edits can be detected.
    pub fn save_atomic(&self) -> Result<(), ConfigError> {
        if self.ephemeral {
            return Ok(());
        }
        self.save_to_path(&paths::config_path()?)
    }

    /// Atomically save to `path`, preserving comments in its current content.
    /// Used by tests and one-shot tools; long-lived writers should use
    /// [`ConfigFile::save`].
    pub fn save_to_path(&self, path: &Path) -> Result<(), ConfigError> {
        let (_, mut file) = ConfigFile::load_from_path(path)?;
        file.save(self)
    }
}

/// Parse `source`, applying every migration its declared `schema_version`
/// needs. Returns the migrated config plus the version the file actually
/// declared, so callers can tell a migrated load from an already-current one.
fn parse_config(path: &Path, source: &str) -> Result<(Config, u32), ConfigError> {
    let header: ConfigHeader = toml::from_str(source).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source: Box::new(source),
    })?;
    if header.schema_version == 0 || header.schema_version > SCHEMA_VERSION {
        return Err(ConfigError::UnsupportedSchemaVersion {
            path: path.to_path_buf(),
            found: header.schema_version,
        });
    }
    reject_obsolete_fields(path, source, header.schema_version)?;
    let mut config: Config = toml::from_str(source).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source: Box::new(source),
    })?;
    if header.schema_version <= 3 {
        config.migrate_owner_locked_gestures();
    }
    // v5 is the schema this change is part of, so every *released* schema —
    // v4 and below — is what needs the rename. A file already declaring v5 was
    // written by an unpublished build of this branch and is not something users
    // have on disk.
    if header.schema_version <= 4 {
        config.migrate_transport_scoped_keys();
    }
    // Every released schema may contain an explicit copy of the pre-v7
    // thumb-wheel defaults, either globally or in a per-app profile.
    if header.schema_version <= 6 {
        config.migrate_thumbwheel_native_direction();
    }
    config.repair_duplicate_routes();
    config.schema_version = SCHEMA_VERSION;
    Ok((config, header.schema_version))
}

fn reject_obsolete_fields(path: &Path, source: &str, version: u32) -> Result<(), ConfigError> {
    let value: toml::Value = toml::from_str(source).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source: Box::new(source),
    })?;
    let Some(devices) = value.get("devices").and_then(toml::Value::as_table) else {
        return Ok(());
    };
    for (device_key, value) in devices {
        let Some(device) = value.as_table() else {
            continue;
        };
        for (field, last_version) in [
            ("button_bindings", 1),
            ("gesture_bindings", 1),
            ("gesture_owner", 3),
        ] {
            if version > last_version && device.contains_key(field) {
                return Err(ConfigError::ObsoleteField {
                    path: path.to_path_buf(),
                    field: format!("devices.{device_key}.{field}"),
                    version,
                });
            }
        }
    }
    Ok(())
}

fn render_config(
    config: &Config,
    original: Option<&str>,
    path: &Path,
) -> Result<String, ConfigError> {
    let generated = toml::to_string_pretty(config)?;
    let Some(original) = original else {
        return Ok(generated);
    };
    let mut document = original
        .parse::<DocumentMut>()
        .map_err(|source| ConfigError::Edit {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
    let generated = generated
        .parse::<DocumentMut>()
        .map_err(|source| ConfigError::Edit {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
    reconcile_table(document.as_table_mut(), generated.as_table());
    Ok(document.to_string())
}

fn reconcile_table(current: &mut Table, generated: &Table) {
    let stale: Vec<String> = current
        .iter()
        .filter(|(key, _)| generated.get(key).is_none())
        .map(|(key, _)| key.to_string())
        .collect();
    for key in stale {
        current.remove(&key);
    }
    for (key, generated_item) in generated {
        if let Some(current_item) = current.get_mut(key) {
            reconcile_item(current_item, generated_item);
        } else {
            current.insert(key, generated_item.clone());
        }
    }
}

fn reconcile_item(current: &mut Item, generated: &Item) {
    if let (Some(current), Some(generated)) = (current.as_table_mut(), generated.as_table()) {
        reconcile_table(current, generated);
        return;
    }
    let decor = current.as_value().map(|value| value.decor().clone());
    *current = generated.clone();
    if let (Some(decor), Some(value)) = (decor, current.as_value_mut()) {
        *value.decor_mut() = decor;
    }
}

fn backup_config_once(path: &Path) -> io::Result<()> {
    let mut backed_up = BACKED_UP_CONFIGS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if backed_up.contains(path) {
        return Ok(());
    }
    match fs::metadata(path) {
        Ok(_) => backup_existing_config(path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    backed_up.insert(path.to_path_buf());
    Ok(())
}

pub(super) fn backup_existing_config(path: &Path) -> io::Result<()> {
    for generation in (1..CONFIG_BACKUP_GENERATIONS).rev() {
        let source = config_backup_path(path, generation)?;
        match fs::read(&source) {
            Ok(bytes) => write_atomic(&config_backup_path(path, generation + 1)?, &bytes)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    write_atomic(&config_backup_path(path, 1)?, &fs::read(path)?)
}

/// Path of the pre-migration copy: the config's own name with
/// `.v<version>.bak` appended, so `config.toml` yields
/// `config.toml.v4.bak`. Appended, not substituted — `with_extension` would
/// replace `.toml` and hand back `config.v4.bak`, which no longer names the
/// file it is a copy of.
pub(super) fn migration_backup_path(path: &Path, version: u32) -> io::Result<PathBuf> {
    let Some(file_name) = path.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "config path has no file name",
        ));
    };
    let mut backup_name = OsString::from(file_name);
    backup_name.push(format!(".v{version}.bak"));
    Ok(path.with_file_name(backup_name))
}

pub(super) fn config_backup_path(path: &Path, generation: usize) -> io::Result<PathBuf> {
    let Some(file_name) = path.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "config path has no file name",
        ));
    };
    let mut backup_name = OsString::from(file_name);
    backup_name.push(format!(".backup.{generation}"));
    Ok(path.with_file_name(backup_name))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    #[cfg_attr(
        not(unix),
        expect(unused_mut, reason = "only the unix path mutates the options")
    )]
    let mut options = AtomicWriteFile::options();
    #[cfg(unix)]
    {
        use atomic_write_file::unix::OpenOptionsExt as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        options.preserve_mode(false).mode(0o600);
    }
    let mut file = options.open(path)?;
    io::Write::write_all(&mut file, bytes)?;
    file.commit()
}
