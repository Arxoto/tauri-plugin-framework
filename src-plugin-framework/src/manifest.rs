//! Plugin manifests (`manifest.json` on disk, built in code for Rust plugins).

use std::path::{Path, PathBuf};
use std::fs;

use serde::{Deserialize, Serialize};

use crate::error::{PluginError, PluginResult};

pub const MANIFEST_FILE: &str = "manifest.json";
pub const DEFAULT_ENTRY: &str = "index.js";
pub const DEFAULT_RUNTIME: &str = "worker";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    /// Implemented in Rust inside the application binary.
    Rust,
    /// Declared by a `manifest.json` + entry script on disk.
    Js,
}

impl PluginKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PluginKind::Rust => "rust",
            PluginKind::Js => "js",
        }
    }
}

/// Everything the framework knows about a plugin.
///
/// Rust plugins build this in code (see [`PluginManifest::rust`]), JS plugins
/// deserialize it from `manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default = "default_kind")]
    pub kind: PluginKind,
    #[serde(default = "default_entry")]
    pub entry: String,
    #[serde(default = "default_runtime")]
    pub runtime: String,
    /// Methods the plugin says it exposes. Purely informative for the UI:
    /// the framework never enforces it, calling an unknown method is a plugin
    /// level error.
    #[serde(default)]
    pub methods: Vec<String>,
    /// Capabilities the plugin may use through its context.
    ///
    /// Known values: `log`, `kv`, `emit`, `call`, or `*` for everything.
    /// JS plugins default to `["log"]`; Rust plugins are trusted.
    #[serde(default)]
    pub permissions: Vec<String>,
}

fn default_kind() -> PluginKind {
    PluginKind::Js
}

fn default_entry() -> String {
    DEFAULT_ENTRY.to_string()
}

fn default_runtime() -> String {
    DEFAULT_RUNTIME.to_string()
}

impl PluginManifest {
    /// Manifest for a plugin that is compiled into the application.
    pub fn rust(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            version: "0.1.0".to_string(),
            description: String::new(),
            author: String::new(),
            kind: PluginKind::Rust,
            entry: String::new(),
            runtime: "in-process".to_string(),
            methods: Vec::new(),
            permissions: Vec::new(),
        }
    }

    /// Manifest for a plugin loaded from a directory.
    pub fn js(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            version: "0.1.0".to_string(),
            description: String::new(),
            author: String::new(),
            kind: PluginKind::Js,
            entry: DEFAULT_ENTRY.to_string(),
            runtime: DEFAULT_RUNTIME.to_string(),
            methods: Vec::new(),
            permissions: vec!["log".to_string()],
        }
    }

    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = author.into();
        self
    }

    pub fn with_methods<I, S>(mut self, methods: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.methods = methods.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_permissions<I, S>(mut self, permissions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.permissions = permissions.into_iter().map(Into::into).collect();
        self
    }

    pub fn is_js(&self) -> bool {
        self.kind == PluginKind::Js
    }

    /// Reads `manifest.json` and the entry script from `dir`.
    ///
    /// The source is returned alongside the manifest because the runtime ships
    /// the script to the webview, which is the only place a Worker can be
    /// created from.
    pub fn load_from_dir(dir: &Path) -> PluginResult<(Self, String, PathBuf)> {
        let manifest_path = dir.join(MANIFEST_FILE);
        let display = manifest_path.display().to_string();
        let raw = fs::read_to_string(&manifest_path).map_err(|e| PluginError::io(&display, e))?;
        let manifest: PluginManifest = serde_json::from_str(&raw)
            .map_err(|e| PluginError::invalid_manifest(&display, e.to_string()))?;
        manifest.validate(&display)?;

        let entry_path = dir.join(&manifest.entry);
        let entry_display = entry_path.display().to_string();
        let source = fs::read_to_string(&entry_path)
            .map_err(|e| PluginError::io(&entry_display, e))?;

        Ok((manifest, source, entry_path))
    }

    pub fn validate(&self, origin: &str) -> PluginResult<()> {
        if self.id.trim().is_empty() {
            return Err(PluginError::invalid_manifest(origin, "`id` is required"));
        }
        if !self.is_js() {
            return Err(PluginError::invalid_manifest(
                origin,
                "only `kind: \"js\"` plugins can be loaded from disk",
            ));
        }
        if self.entry.trim().is_empty() {
            return Err(PluginError::invalid_manifest(
                origin,
                "`entry` must point at the plugin script",
            ));
        }
        if self.runtime != DEFAULT_RUNTIME {
            return Err(PluginError::invalid_manifest(
                origin,
                format!(
                    "unsupported runtime `{}`, this framework only ships `{DEFAULT_RUNTIME}`",
                    self.runtime
                ),
            ));
        }
        Ok(())
    }

    pub fn entry_path(&self, dir: &Path) -> PathBuf {
        dir.join(&self.entry)
    }
}
