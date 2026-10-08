//! Live configuration, persistence, and rollback state.

use std::ops::Deref;

use openlogi_core::config::{Config, ConfigError, ConfigFile, RecoveryPlan};
use tracing::warn;

/// Where [`super::AppState`] may persist configuration mutations.
///
/// Runtime state uses [`Self::UserFile`]. Tests opt into
/// [`Self::MemoryOnly`] so realistic device fixtures can never modify the
/// developer's actual `config.toml`.
#[derive(Debug, Clone)]
pub enum ConfigPersistence {
    /// Persist through the tracked user file, preserving comments and refusing
    /// to overwrite edits made after startup.
    UserFile(ConfigFile),
    /// A load error made the config unsafe to write for this process lifetime.
    ReadOnly(String),
    /// A recovery replaced the file; only a fresh process may edit it now.
    Restored,
    /// A confirmed plan is being applied off the UI thread.
    Recovering(std::path::PathBuf),
    /// A restore failed, possibly after replacement but before directory sync.
    /// Keep the tracked path so a fresh preview can safely retry.
    RecoveryPaused(std::path::PathBuf),
    /// Keep changes in the in-memory [`Config`] only.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "test-only persistence boundary")
    )]
    MemoryOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfigIssue {
    Persistence(String),
    Reload(String),
}

impl ConfigIssue {
    fn message(&self) -> &str {
        match self {
            Self::Persistence(message) | Self::Reload(message) => message,
        }
    }
}

/// Owns the live and last-persisted revisions as one rollback boundary.
pub(super) struct ConfigState {
    current: Config,
    persisted: Config,
    persistence: ConfigPersistence,
    issue: Option<ConfigIssue>,
}

impl ConfigState {
    pub(super) fn new(current: Config, persistence: ConfigPersistence) -> Self {
        let issue = match &persistence {
            ConfigPersistence::ReadOnly(error) => Some(ConfigIssue::Persistence(error.clone())),
            ConfigPersistence::UserFile(_)
            | ConfigPersistence::MemoryOnly
            | ConfigPersistence::Restored
            | ConfigPersistence::Recovering(_)
            | ConfigPersistence::RecoveryPaused(_) => None,
        };
        let persisted = current.clone();
        Self {
            current,
            persisted,
            persistence,
            issue,
        }
    }

    pub(super) fn issue(&self) -> Option<&str> {
        self.issue.as_ref().map(ConfigIssue::message)
    }

    pub(super) fn should_reload_agent(&self) -> bool {
        self.issue.is_none() && matches!(&self.persistence, ConfigPersistence::UserFile(_))
    }

    pub(super) fn recovery_path(&self) -> Result<std::path::PathBuf, ConfigError> {
        match &self.persistence {
            ConfigPersistence::UserFile(file) => Ok(file.path().to_path_buf()),
            ConfigPersistence::RecoveryPaused(path) => Ok(path.clone()),
            ConfigPersistence::ReadOnly(_) => Ok(openlogi_core::paths::config_path()?),
            ConfigPersistence::MemoryOnly
            | ConfigPersistence::Restored
            | ConfigPersistence::Recovering(_) => Err(ConfigError::Read {
                path: std::path::PathBuf::new(),
                source: std::io::Error::other(
                    "configuration recovery is unavailable in this session",
                ),
            }),
        }
    }

    pub(super) fn restored(&self) -> bool {
        matches!(self.persistence, ConfigPersistence::Restored)
    }

    pub(super) fn recovery_freezes_writes(&self) -> bool {
        matches!(
            self.persistence,
            ConfigPersistence::Restored
                | ConfigPersistence::Recovering(_)
                | ConfigPersistence::RecoveryPaused(_)
        )
    }

    pub(super) fn recovering(&self) -> bool {
        matches!(self.persistence, ConfigPersistence::Recovering(_))
    }

    pub(super) fn begin_recovery(&mut self, plan: &RecoveryPlan) -> Result<(), ConfigError> {
        let target = self.recovery_path()?;
        if plan.target() != target {
            return Err(ConfigError::Conflict { path: target });
        }
        // Freeze before spawning: delayed UI and device callbacks must not save
        // the old in-memory revision while the atomic writer is running.
        self.persistence = ConfigPersistence::Recovering(target);
        self.issue = None;
        Ok(())
    }

    pub(super) fn finish_recovery(&mut self, result: &Result<(), ConfigError>) {
        let ConfigPersistence::Recovering(target) = &self.persistence else {
            return;
        };
        match result {
            Ok(()) => {
                self.persistence = ConfigPersistence::Restored;
                self.issue = None;
            }
            Err(error) => {
                // Directory sync may fail after replacement. Keep writes frozen
                // until a fresh preview or process resolves the on-disk revision.
                self.persistence = ConfigPersistence::RecoveryPaused(target.clone());
                self.issue = Some(ConfigIssue::Persistence(error.to_string()));
            }
        }
    }

    #[cfg(test)]
    fn recover(&mut self, plan: RecoveryPlan) -> Result<(), ConfigError> {
        self.begin_recovery(&plan)?;
        let result = plan.apply();
        self.finish_recovery(&result);
        result
    }

    /// Scope an uncommitted edit to this rollback boundary. Runtime callers
    /// must follow it with the appropriate `AppState` persistence path; startup
    /// migration and tests are the only intentional in-memory-only callers.
    pub(super) fn edit<R>(&mut self, edit: impl FnOnce(&mut Config) -> R) -> R {
        edit(&mut self.current)
    }

    /// Persist the live revision, restoring the last persisted one on failure.
    pub(super) fn persist(&mut self, what: &str) -> bool {
        let result = match &mut self.persistence {
            ConfigPersistence::UserFile(file) => file.save(&self.current),
            ConfigPersistence::ReadOnly(_)
            | ConfigPersistence::Restored
            | ConfigPersistence::Recovering(_)
            | ConfigPersistence::RecoveryPaused(_) => {
                self.restore();
                return false;
            }
            ConfigPersistence::MemoryOnly => Ok(()),
        };
        if let Err(error) = result {
            warn!(error = %error, what, "could not persist to config.toml");
            self.issue = Some(ConfigIssue::Persistence(error.to_string()));
            self.restore();
            return false;
        }
        self.persisted.clone_from(&self.current);
        if matches!(&self.issue, Some(ConfigIssue::Persistence(_))) {
            self.issue = None;
        }
        true
    }

    pub(super) fn apply_reload_result(
        &mut self,
        result: Result<(), openlogi_ipc::ConfigReloadError>,
    ) -> bool {
        if self.recovery_freezes_writes() {
            return false;
        }
        let next = match result {
            Err(error) => Some(ConfigIssue::Reload(error.message)),
            Ok(()) if matches!(&self.issue, Some(ConfigIssue::Reload(_))) => None,
            Ok(()) => return false,
        };
        if self.issue == next {
            return false;
        }
        self.issue = next;
        true
    }

    fn restore(&mut self) {
        self.current.clone_from(&self.persisted);
    }
}

impl Deref for ConfigState {
    type Target = Config;

    fn deref(&self) -> &Self::Target {
        &self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_freezes_writes_before_background_application() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        let source = dir.path().join("restore.toml");
        Config::default().save_to_path(&path).unwrap();
        Config::default().save_to_path(&source).unwrap();
        let original = std::fs::read(&path).unwrap();
        let (config, file) = ConfigFile::load_from_path(&path).unwrap();
        let mut state = ConfigState::new(config, ConfigPersistence::UserFile(file));
        let plan = RecoveryPlan::prepare(&path, &source).unwrap();
        state.begin_recovery(&plan).unwrap();
        assert!(state.recovering());
        assert!(!state.should_reload_agent());
        state.edit(|config| config.app_settings.launch_at_login = false);
        assert!(!state.persist("late callback"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(
            state.begin_recovery(&plan).is_err(),
            "a second writer must not start"
        );
        let result = plan.apply();
        state.finish_recovery(&result);
        result.unwrap();
        assert!(state.restored());
        assert!(!state.recovering());
    }

    #[test]
    fn failed_recovery_freezes_writes_until_a_fresh_preview_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        let source = dir.path().join("restore.toml");
        Config::default().save_to_path(&path).unwrap();
        Config::default().save_to_path(&source).unwrap();
        let (config, file) = ConfigFile::load_from_path(&path).unwrap();
        let mut state = ConfigState::new(config, ConfigPersistence::UserFile(file));
        let plan = RecoveryPlan::prepare(&path, &source).unwrap();
        let changed = format!(
            "# External edit\n{}",
            std::fs::read_to_string(&path).unwrap()
        );
        std::fs::write(&path, &changed).unwrap();
        state.recover(plan).unwrap_err();
        assert!(!state.restored());
        assert!(state.issue().is_some());
        assert!(!state.should_reload_agent());
        state.edit(|config| config.app_settings.launch_at_login = false);
        assert!(!state.persist("old settings window"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), changed);
        assert_eq!(state.recovery_path().unwrap(), path);
        state
            .recover(RecoveryPlan::prepare(&path, &source).unwrap())
            .unwrap();
        assert!(state.restored());
        assert!(state.issue().is_none());
    }

    #[test]
    fn recovery_freezes_old_writes_and_ignores_a_delayed_reload_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        let source = dir.path().join("restore.toml");
        let mut old = Config::default();
        old.app_settings.launch_at_login = false;
        old.save_to_path(&path).unwrap();
        let (config, file) = ConfigFile::load_from_path(&path).unwrap();
        let mut state = ConfigState::new(config, ConfigPersistence::UserFile(file));
        let mut restored = old.clone();
        restored.app_settings.launch_at_login = true;
        restored.save_to_path(&source).unwrap();
        state
            .recover(RecoveryPlan::prepare(&path, &source).unwrap())
            .unwrap();
        assert!(state.restored());
        assert!(!state.should_reload_agent());
        assert!(
            !state.apply_reload_result(Err(openlogi_ipc::ConfigReloadError {
                message: "old reply".into()
            }))
        );
        state.edit(|config| config.app_settings.launch_at_login = false);
        assert!(!state.persist("stale settings window"));
        assert!(
            Config::load_from_path(&path)
                .unwrap()
                .app_settings
                .launch_at_login
        );
        assert!(state.issue().is_none());
    }
}
