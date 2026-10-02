//! The context handed to plugin implementations.
//!
//! It is the only door between a plugin and the framework: every method here
//! is a "capability". For JS plugins the very same capabilities are exposed to
//! the Worker through the shim, and each one is checked against the manifest
//! `permissions` before it runs.

use std::sync::Arc;

use serde_json::Value;

use crate::error::PluginResult;
use crate::host::PluginHost;

/// How a log line was produced. Kept as a string so plugins can add levels
/// without touching the framework.
pub mod level {
    pub const DEBUG: &str = "debug";
    pub const INFO: &str = "info";
    pub const WARN: &str = "warn";
    pub const ERROR: &str = "error";
}

/// Cheap to clone: a plugin handle plus the owning plugin id.
#[derive(Clone)]
pub struct PluginCtx {
    host: PluginHost,
    plugin_id: Arc<str>,
}

impl PluginCtx {
    pub(crate) fn new(host: PluginHost, plugin_id: impl Into<Arc<str>>) -> Self {
        Self {
            host,
            plugin_id: plugin_id.into(),
        }
    }

    /// Id of the plugin this context was created for.
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn host(&self) -> &PluginHost {
        &self.host
    }

    // ---------------------------------------------------------------- logging

    pub fn log(&self, level: &str, message: impl Into<String>) -> PluginResult<()> {
        self.check("log")?;
        self.host
            .push_log(self.plugin_id(), level, message.into(), None);
        Ok(())
    }

    pub fn debug(&self, message: impl Into<String>) -> PluginResult<()> {
        self.log(level::DEBUG, message)
    }

    pub fn info(&self, message: impl Into<String>) -> PluginResult<()> {
        self.log(level::INFO, message)
    }

    pub fn warn(&self, message: impl Into<String>) -> PluginResult<()> {
        self.log(level::WARN, message)
    }

    pub fn error(&self, message: impl Into<String>) -> PluginResult<()> {
        self.log(level::ERROR, message)
    }

    // -------------------------------------------------------------- shared kv

    pub fn kv_get(&self, key: &str) -> PluginResult<Option<Value>> {
        self.check("kv")?;
        Ok(self.host.kv_get(key))
    }

    pub fn kv_set(&self, key: &str, value: Value) -> PluginResult<()> {
        self.check("kv")?;
        self.host.kv_set(key, value);
        Ok(())
    }

    pub fn kv_delete(&self, key: &str) -> PluginResult<()> {
        self.check("kv")?;
        self.host.kv_delete(key);
        Ok(())
    }

    pub fn kv_keys(&self) -> PluginResult<Vec<String>> {
        self.check("kv")?;
        Ok(self.host.kv_keys())
    }

    // ----------------------------------------------------------------- events

    /// Broadcasts a framework event. The demo UI (and any other frontend
    /// listener) receives it, which is how a plugin pushes data outward
    /// without knowing who is listening.
    pub fn emit(&self, event: &str, payload: Value) -> PluginResult<()> {
        self.check("emit")?;
        self.host.emit_plugin_event(self.plugin_id(), event, payload);
        Ok(())
    }

    // ------------------------------------------------------------ cross calls

    /// Calls another plugin (Rust or JS) and awaits its answer.
    ///
    /// This is the same path the framework itself uses, so a Rust plugin and a
    /// JS plugin can call each other freely.
    pub async fn call_plugin(
        &self,
        plugin_id: &str,
        method: &str,
        params: Value,
    ) -> PluginResult<Value> {
        self.check("call")?;
        self.host
            .invoke(Some(self.plugin_id()), plugin_id, method, params)
            .await
    }

    // -------------------------------------------------------------- internals

    pub(crate) fn check(&self, capability: &str) -> PluginResult<()> {
        self.host.check_permission(self.plugin_id(), capability)
    }

    pub(crate) fn from_parts(host: PluginHost, plugin_id: impl Into<Arc<str>>) -> Self {
        Self::new(host, plugin_id)
    }
}
