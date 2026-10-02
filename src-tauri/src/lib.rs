//! Feasibility playground for a runtime plugin framework.
//!
//! * `plugin_framework` (a separate crate, `src-plugin-framework`) owns the
//!   registry, the capabilities and the Rust <-> Worker bridge.
//! * `builtin` holds plugins compiled into this binary.
//! * JS plugins are discovered at runtime inside `plugins/`.
//!
//! See `docs/architecture.md`.

mod builtin;
mod plugin_ipc;
mod tauri_bridge;

use plugin_framework::PluginHost;
use std::sync::Arc;
use tauri_bridge::{TauriBridge, TauriSpawner};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let host = PluginHost::new();
    if let Err(error) = builtin::register(&host) {
        eprintln!("failed to register built-in plugins: {error}");
    }
    let setup_host = host.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(host)
        .invoke_handler(tauri::generate_handler![
            plugin_ipc::framework_info,
            plugin_ipc::plugin_list,
            plugin_ipc::plugin_invoke,
            plugin_ipc::plugin_rescan,
            plugin_ipc::plugin_runtime_ready,
            plugin_ipc::plugin_ctx_call,
            plugin_ipc::plugin_runtime_reply,
            plugin_ipc::plugin_runtime_status,
            plugin_ipc::plugin_runtime_log,
            plugin_ipc::plugin_logs,
            plugin_ipc::plugin_events,
        ])
        .setup(move |app| {
            // Give the framework a way to reach the webview, then scan the
            // plugin directory. JS plugins are handed to the webview only once
            // it reports that it can create Workers.
            setup_host.set_bridge(Arc::new(TauriBridge::new(app.handle().clone())));
            setup_host.set_spawner(Arc::new(TauriSpawner));
            if let Err(error) = setup_host.bootstrap() {
                eprintln!("plugin bootstrap failed: {error}");
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
