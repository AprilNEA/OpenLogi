//! Live configuration, persistence, and rollback state.

use std::ops::Deref;

use openlogi_core::config::{Config, ConfigFile};
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
            ConfigPersistence::UserFile(_) | ConfigPersistence::MemoryOnly => None,
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
            ConfigPersistence::ReadOnly(_) => {
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
