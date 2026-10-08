//! Validated device descriptors and sandboxed component contracts.
//!
//! Parsing does not execute code or access devices. The optional runner is
//! linked by the agent, never by the desktop or overlay.

#![deny(missing_docs)]

pub mod descriptor;
pub mod manifest;
#[cfg(feature = "packages")]
pub mod package;
#[cfg(feature = "runner")]
pub mod runtime;
pub mod selector;

/// Limits shared by descriptor and package admission.
pub mod limits {
    /// Maximum descriptor or manifest size.
    pub const DESCRIPTOR_BYTES: usize = 64 * 1024;
    /// Maximum selector alternatives in one descriptor.
    pub const SELECTORS: usize = 64;
    /// Maximum controls or fields in one contract.
    pub const FIELDS: usize = 128;
    /// Maximum input component size.
    pub const COMPONENT_BYTES: usize = 8 * 1024 * 1024;
    /// Maximum referenced package content size.
    pub const PACKAGE_BYTES: usize = 16 * 1024 * 1024;
}

/// A source rejected before hardware access or activation.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// TOML syntax or a common field is invalid.
    #[error("invalid descriptor or manifest: {0}")]
    Parse(#[from] toml::de::Error),
    /// A structurally valid source violates the host contract.
    #[error("invalid plugin contract: {0}")]
    Invalid(String),
    /// Package I/O failed.
    #[error("plugin file operation failed: {0}")]
    Io(#[from] std::io::Error),
}

impl From<openlogi_core::peripheral::IdentifierError> for PluginError {
    fn from(value: openlogi_core::peripheral::IdentifierError) -> Self {
        Self::Invalid(value.to_string())
    }
}

/// Admit only component binaries and bounded module structure before compilation.
pub fn validate_component(bytes: &[u8]) -> Result<(), PluginError> {
    if bytes.len() > limits::COMPONENT_BYTES || !wasmparser::Parser::is_component(bytes) {
        return Err(PluginError::Invalid(
            "expected an at most 8 MiB Wasm component, not a serialized runtime artifact".into(),
        ));
    }
    let mut modules = 0;
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        let payload = payload.map_err(|e| PluginError::Invalid(e.to_string()))?;
        if matches!(
            payload,
            wasmparser::Payload::ModuleSection { .. }
                | wasmparser::Payload::ComponentSection { .. }
        ) {
            modules += 1;
            if modules > 32 {
                return Err(PluginError::Invalid(
                    "component contains more than 32 nested modules/components".into(),
                ));
            }
        }
    }
    Ok(())
}
