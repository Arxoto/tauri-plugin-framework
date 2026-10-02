//! Error type shared by the framework and by plugin implementations.

use std::fmt;

use serde::{Deserialize, Serialize};

pub type PluginResult<T> = Result<T, PluginError>;

/// A serializable error.
///
/// It is intentionally small (a stable `code` plus a human readable `message`)
/// so that it can cross the Rust <-> JS boundary without losing information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginError {
    pub code: String,
    pub message: String,
}

impl PluginError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn message(message: impl Into<String>) -> Self {
        Self::new("plugin_error", message)
    }

    pub fn not_found(plugin_id: &str) -> Self {
        Self::new(
            "plugin_not_found",
            format!("plugin `{plugin_id}` is not registered"),
        )
    }

    pub fn unknown_method(plugin_id: &str, method: &str) -> Self {
        Self::new(
            "unknown_method",
            format!("plugin `{plugin_id}` does not expose a method named `{method}`"),
        )
    }

    pub fn unknown_capability(capability: &str) -> Self {
        Self::new(
            "unknown_capability",
            format!("`{capability}` is not a capability provided by this framework"),
        )
    }

    pub fn denied(plugin_id: &str, capability: &str) -> Self {
        Self::new(
            "capability_denied",
            format!("plugin `{plugin_id}` is not allowed to use the `{capability}` capability"),
        )
    }

    pub fn invalid_manifest(path: &str, reason: impl Into<String>) -> Self {
        Self::new(
            "invalid_manifest",
            format!("invalid manifest `{path}`: {}", reason.into()),
        )
    }

    pub fn io(path: &str, error: impl fmt::Display) -> Self {
        Self::new("io_error", format!("{path}: {error}"))
    }

    pub fn runtime_unavailable() -> Self {
        Self::new(
            "js_runtime_unavailable",
            "the JS plugin runtime is not ready yet (call `plugin_runtime_ready` first)",
        )
    }

    pub fn timeout(plugin_id: &str, method: &str, ms: u64) -> Self {
        Self::new(
            "timeout",
            format!("plugin `{plugin_id}` did not answer `{method}` within {ms}ms"),
        )
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new("invalid_argument", message)
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for PluginError {}
