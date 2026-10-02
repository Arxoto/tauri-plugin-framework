//! The Tauri side of the two framework seams.
//!
//! `plugin-framework` itself has no Tauri dependency: it emits through a
//! [`UiBridge`] and spawns through a [`TaskSpawner`]. These two impls are the
//! whole adapter.

use plugin_framework::{BoxFuture, TaskSpawner, UiBridge};
use serde_json::Value;
use tauri::{AppHandle, Emitter};

/// Forwards framework events to every webview, which is how the JS plugin
/// runtime receives its Worker instructions.
pub struct TauriBridge {
    app: AppHandle,
}

impl TauriBridge {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl UiBridge for TauriBridge {
    fn emit(&self, event: &str, payload: Value) {
        if let Err(error) = self.app.emit(event, payload) {
            eprintln!("[plugin-framework] failed to emit `{event}`: {error}");
        }
    }
}

/// Runs framework background work on the Tauri async runtime.
pub struct TauriSpawner;

impl TaskSpawner for TauriSpawner {
    fn spawn(&self, task: BoxFuture) {
        tauri::async_runtime::spawn(task);
    }
}
