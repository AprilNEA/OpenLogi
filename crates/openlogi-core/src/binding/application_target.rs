//! Validated operating-system launch targets.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Why an application launch target could not be constructed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum ApplicationTargetError {
    /// The launch path or URL was blank.
    #[error("application path must not be empty")]
    EmptyPath,
}

/// Validated application, folder, filesystem, or URL target.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "ApplicationTargetWire", into = "ApplicationTargetWire")]
pub struct ApplicationTarget {
    path: String,
    display_name: String,
}

impl ApplicationTarget {
    /// Validate a platform path or URL and its user-facing name.
    pub fn new(
        path: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Result<Self, ApplicationTargetError> {
        let path = path.into().trim().to_string();
        if path.is_empty() {
            return Err(ApplicationTargetError::EmptyPath);
        }
        let requested_name = display_name.into();
        let display_name = if requested_name.trim().is_empty() {
            std::path::Path::new(&path)
                .file_stem()
                .and_then(std::ffi::OsStr::to_str)
                .filter(|name| !name.is_empty())
                .unwrap_or(path.as_str())
                .to_string()
        } else {
            requested_name.trim().to_string()
        };
        Ok(Self { path, display_name })
    }

    /// Platform path or URL passed to the operating system. A leading `~` is
    /// expanded by the injector immediately before opening.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Name displayed in configuration and overlay UI.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplicationTargetWire {
    path: String,
    #[serde(default)]
    display_name: String,
}

impl TryFrom<ApplicationTargetWire> for ApplicationTarget {
    type Error = ApplicationTargetError;

    fn try_from(target: ApplicationTargetWire) -> Result<Self, Self::Error> {
        Self::new(target.path, target.display_name)
    }
}

impl From<ApplicationTarget> for ApplicationTargetWire {
    fn from(target: ApplicationTarget) -> Self {
        Self {
            path: target.path,
            display_name: target.display_name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_and_derives_display_name() {
        let target =
            ApplicationTarget::new("/Applications/Safari.app", "").expect("valid target failed");
        assert_eq!(target.path(), "/Applications/Safari.app");
        assert_eq!(target.display_name(), "Safari");
        assert_eq!(
            ApplicationTarget::new("  ", "Safari"),
            Err(ApplicationTargetError::EmptyPath)
        );
    }

    #[test]
    fn roundtrips_as_a_human_readable_target_table() {
        let target =
            ApplicationTarget::new("https://example.test", "Example").expect("valid target failed");
        let encoded = toml::to_string(&target).expect("target serialization failed");
        assert!(encoded.contains("path = \"https://example.test\""));
        assert_eq!(toml::from_str::<ApplicationTarget>(&encoded), Ok(target));
    }
}
