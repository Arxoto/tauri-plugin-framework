//! The IPC surface of the plugin framework.
//!
//! These commands are deliberately thin: all of the logic lives in
//! `plugin_framework::PluginHost`, this module only adapts `State`/`Result` to
//! Tauri's command model.

use plugin_framework::{
    FrameworkEvent, LogEntry, PluginError, PluginHost, PluginInfo, PluginResult, PLUGIN_DIR_ENV,
    FRAMEWORK_VERSION,
};
use serde_json::{json, Value};
use tauri::State;

/// Everything the UI needs to render the framework header.
#[tauri::command]
pub fn framework_info(host: State<'_, PluginHost>) -> Value {
    json!({
        "version": FRAMEWORK_VERSION,
        "pluginDir": host.resolve_plugin_dir().display().to_string(),
        "pluginDirEnv": PLUGIN_DIR_ENV,
        "runtimeReady": host.runtime_ready(),
        "invokeTimeoutMs": host.invoke_timeout_ms(),
        "capabilities": [
            { "name": "log", "description": "write a line into the framework log" },
            { "name": "kv", "description": "shared key/value store, visible to every plugin" },
            { "name": "emit", "description": "broadcast a framework event to the frontend" },
            { "name": "call", "description": "call a method of another plugin and await the result" },
        ],
    })
}

#[tauri::command]
pub fn plugin_list(host: State<'_, PluginHost>) -> Vec<PluginInfo> {
    host.list()
}

#[tauri::command]
pub fn plugin_logs(host: State<'_, PluginHost>) -> Vec<LogEntry> {
    host.logs()
}

#[tauri::command]
pub fn plugin_events(host: State<'_, PluginHost>) -> Vec<FrameworkEvent> {
    host.events()
}

/// Framework -> plugin call. Works for Rust and JS plugins alike.
#[tauri::command]
pub async fn plugin_invoke(
    host: State<'_, PluginHost>,
    plugin_id: String,
    method: String,
    params: Option<Value>,
) -> PluginResult<Value> {
    host.invoke(None, &plugin_id, &method, params.unwrap_or(Value::Null))
        .await
}

/// Re-scans the plugin directory at runtime.
#[tauri::command]
pub fn plugin_rescan(host: State<'_, PluginHost>) -> PluginResult<Vec<PluginInfo>> {
    host.rescan()
}

/// The webview calls this once it is listening and able to create Workers.
#[tauri::command]
pub fn plugin_runtime_ready(host: State<'_, PluginHost>) -> PluginResult<()> {
    host.mark_runtime_ready();
    Ok(())
}

/// Relayed by the webview: a JS plugin asked for a framework capability.
#[tauri::command]
pub async fn plugin_ctx_call(
    host: State<'_, PluginHost>,
    plugin_id: String,
    capability: String,
    args: Option<Value>,
) -> PluginResult<Value> {
    host.handle_capability(&plugin_id, &capability, args.unwrap_or(Value::Null))
        .await
}

/// Relayed by the webview: a Worker answered a framework initiated call.
#[tauri::command]
pub fn plugin_runtime_reply(
    host: State<'_, PluginHost>,
    call_id: u64,
    ok: bool,
    value: Option<Value>,
    error: Option<String>,
) -> PluginResult<()> {
    let result = if ok {
        Ok(value.unwrap_or(Value::Null))
    } else {
        Err(PluginError::message(
            error.unwrap_or_else(|| "plugin call failed".to_string()),
        ))
    };
    // A `false` return means the watchdog already gave up on this call; the
    // late answer is simply dropped.
    let _ = host.resolve_pending(call_id, result);
    Ok(())
}

/// Relayed by the webview: lifecycle reports coming out of a Worker.
#[tauri::command]
pub fn plugin_runtime_status(
    host: State<'_, PluginHost>,
    plugin_id: String,
    ok: bool,
    message: Option<String>,
) -> PluginResult<()> {
    match (ok, message) {
        (true, Some(message)) => host.push_log(&plugin_id, "info", message, None),
        (true, None) => host.mark_js_loaded(&plugin_id),
        (false, message) => host.mark_js_failed(
            &plugin_id,
            message.unwrap_or_else(|| "worker failed to start".to_string()),
        ),
    }
    Ok(())
}

/// Relayed by the webview: diagnostics from the Worker runtime itself.
///
/// Without this, a Worker that fails to boot is invisible from the Rust side.
#[tauri::command]
pub fn plugin_runtime_log(host: State<'_, PluginHost>, level: String, message: String) {
    host.push_log("js-runtime", &level, message, None);
}
