//! The registry: owns plugins, routes calls, and provides the capabilities.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{env, fs};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::bridge::{BoxFuture, NullBridge, TaskSpawner, ThreadSpawner, UiBridge};
use crate::ctx::PluginCtx;
use crate::error::{PluginError, PluginResult};
use crate::manifest::{PluginKind, PluginManifest, MANIFEST_FILE};
use crate::oneshot;
use crate::rust_plugin::RustPlugin;

/// Emitted with the full plugin list whenever the registry changes.
pub const EVENT_PLUGINS_CHANGED: &str = "framework://plugins";
/// Emitted for every `ctx.log(...)`.
pub const EVENT_LOG: &str = "framework://log";
/// Emitted for every `ctx.emit(...)`.
pub const EVENT_PLUGIN_EVENT: &str = "framework://event";
/// Tells the webview to spin up (or replace) the Worker of a JS plugin.
pub const EVENT_JS_LOAD: &str = "framework://js-load";
/// Tells the webview to tear a Worker down.
pub const EVENT_JS_UNLOAD: &str = "framework://js-unload";
/// Asks the webview to forward a call into a plugin Worker.
pub const EVENT_JS_INVOKE: &str = "framework://js-invoke";
/// Emitted when the JS runtime reports itself ready.
pub const EVENT_RUNTIME_STATUS: &str = "framework://runtime-status";

/// Environment variable that overrides plugin directory discovery.
pub const PLUGIN_DIR_ENV: &str = "PLUGIN_FRAMEWORK_DIR";

const DEFAULT_INVOKE_TIMEOUT_MS: u64 = 10_000;
const RING_CAPACITY: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginState {
    /// Registered but `on_load` has not finished yet.
    Pending,
    Loaded,
    Error,
}

/// Serializable snapshot of a plugin, used by the UI.
#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub kind: PluginKind,
    pub runtime: String,
    pub entry: String,
    pub methods: Vec<String>,
    pub permissions: Vec<String>,
    /// Directory of a JS plugin, or `builtin` for Rust plugins.
    pub origin: String,
    pub state: PluginState,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub source: String,
    pub level: String,
    pub message: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FrameworkEvent {
    pub source: String,
    pub event: String,
    pub payload: Value,
    pub timestamp: u64,
}

/// Payload of [`EVENT_JS_LOAD`].
#[derive(Debug, Clone, Serialize)]
pub struct JsLoadPayload {
    pub id: String,
    pub manifest: PluginManifest,
    /// Source of the entry script, injected into the plugin's Worker.
    pub source: String,
    pub entry_path: String,
}

struct JsPlugin {
    entry_path: PathBuf,
    source: String,
}

enum PluginImpl {
    Rust(Arc<dyn RustPlugin>),
    Js(JsPlugin),
}

pub(crate) struct LoadedPlugin {
    manifest: PluginManifest,
    origin: String,
    imp: PluginImpl,
    state: Mutex<(PluginState, Option<String>)>,
}

impl LoadedPlugin {
    fn set_state(&self, state: PluginState, error: Option<String>) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        *guard = (state, error);
    }

    fn info(&self) -> PluginInfo {
        let (state, error) = {
            let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.clone()
        };
        let permissions = if self.manifest.kind == PluginKind::Rust {
            vec!["*".to_string()]
        } else if self.manifest.permissions.is_empty() {
            vec!["log".to_string()]
        } else {
            self.manifest.permissions.clone()
        };
        PluginInfo {
            id: self.manifest.id.clone(),
            name: if self.manifest.name.is_empty() {
                self.manifest.id.clone()
            } else {
                self.manifest.name.clone()
            },
            version: self.manifest.version.clone(),
            description: self.manifest.description.clone(),
            author: self.manifest.author.clone(),
            kind: self.manifest.kind,
            runtime: self.manifest.runtime.clone(),
            entry: self.manifest.entry.clone(),
            methods: self.manifest.methods.clone(),
            permissions,
            origin: self.origin.clone(),
            state,
            error,
        }
    }
}

#[derive(Default)]
struct Inner {
    /// Installed by the host application (in Tauri: a bridge over `AppHandle`).
    bridge: OnceLock<Arc<dyn UiBridge>>,
    /// Installed by the host application (in Tauri: `async_runtime::spawn`).
    spawner: OnceLock<Arc<dyn TaskSpawner>>,
    plugins: RwLock<BTreeMap<String, Arc<LoadedPlugin>>>,
    kv: Mutex<HashMap<String, Value>>,
    logs: Mutex<VecDeque<LogEntry>>,
    events: Mutex<VecDeque<FrameworkEvent>>,
    pending: Mutex<HashMap<u64, oneshot::Sender<PluginResult<Value>>>>,
    call_seq: AtomicU64,
    runtime_ready: AtomicBool,
    plugin_dir: OnceLock<PathBuf>,
    invoke_timeout_ms: AtomicU64,
}

/// Handle to the plugin registry. Clone it freely, it is a thin `Arc`.
#[derive(Clone)]
pub struct PluginHost {
    inner: Arc<Inner>,
}

impl Default for PluginHost {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginHost {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                invoke_timeout_ms: AtomicU64::new(DEFAULT_INVOKE_TIMEOUT_MS),
                ..Default::default()
            }),
        }
    }

    /// How long the framework waits for a JS worker to answer a call.
    /// `0` disables the watchdog.
    pub fn set_invoke_timeout_ms(&self, ms: u64) {
        self.inner.invoke_timeout_ms.store(ms, Ordering::Relaxed);
    }

    pub fn invoke_timeout_ms(&self) -> u64 {
        self.inner.invoke_timeout_ms.load(Ordering::Relaxed)
    }

    /// Gives the framework a way to talk to the UI. Called once at setup.
    ///
    /// Until this is set the framework runs with a no-op bridge: the registry
    /// works, only the events are dropped.
    pub fn set_bridge(&self, bridge: Arc<dyn UiBridge>) {
        if self.inner.bridge.set(bridge).is_err() {
            eprintln!("[plugin-framework] a UI bridge is already installed, ignoring the new one");
        }
    }

    /// Gives the framework a way to run background tasks. Without it, tasks are
    /// driven by the default thread-per-task executor.
    pub fn set_spawner(&self, spawner: Arc<dyn TaskSpawner>) {
        if self.inner.spawner.set(spawner).is_err() {
            eprintln!("[plugin-framework] a task spawner is already installed, ignoring the new one");
        }
    }

    /// The installed bridge, or a throwaway no-op one.
    ///
    /// Note this must not *store* the fallback: plugins can be registered (and
    /// therefore events emitted) before the host application installs its real
    /// bridge, and that early traffic must not lock the slot to the no-op impl.
    fn bridge(&self) -> Arc<dyn UiBridge> {
        match self.inner.bridge.get() {
            Some(bridge) => Arc::clone(bridge),
            None => Arc::new(NullBridge),
        }
    }

    fn spawn_task(&self, task: BoxFuture) {
        match self.inner.spawner.get() {
            Some(spawner) => spawner.spawn(task),
            None => ThreadSpawner.spawn(task),
        }
    }

    // ------------------------------------------------------------- registry

    /// Registers a plugin implemented in Rust.
    pub fn register_rust_plugin(&self, plugin: Arc<dyn RustPlugin>) -> PluginResult<()> {
        let manifest = plugin.manifest().clone();
        if manifest.id.trim().is_empty() {
            return Err(PluginError::message("rust plugin manifest needs an id"));
        }
        let loaded = Arc::new(LoadedPlugin {
            origin: "builtin".to_string(),
            manifest,
            imp: PluginImpl::Rust(plugin),
            state: Mutex::new((PluginState::Pending, None)),
        });
        self.insert(loaded)
    }

    /// Registers every `<dir>/<plugin>/manifest.json` found one level below
    /// `dir`. Bad plugins are logged and skipped instead of aborting the scan.
    pub fn load_plugin_dir(&self, dir: &Path) -> PluginResult<usize> {
        let display = dir.display().to_string();
        let entries = fs::read_dir(dir).map_err(|e| PluginError::io(&display, e))?;
        let mut loaded = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || !path.join(MANIFEST_FILE).is_file() {
                continue;
            }
            match self.load_js_plugin(&path) {
                Ok(()) => loaded += 1,
                Err(error) => {
                    self.push_log("framework", "error", format!("{error}"), None);
                }
            }
        }
        Ok(loaded)
    }

    /// Registers a single JS plugin directory.
    pub fn load_js_plugin(&self, dir: &Path) -> PluginResult<()> {
        let (manifest, source, entry_path) = PluginManifest::load_from_dir(dir)?;
        let id = manifest.id.clone();
        let loaded = Arc::new(LoadedPlugin {
            origin: dir.display().to_string(),
            manifest,
            imp: PluginImpl::Js(JsPlugin {
                entry_path,
                source,
            }),
            state: Mutex::new((PluginState::Pending, None)),
        });
        self.insert(loaded)?;
        self.push_log(
            "framework",
            "info",
            format!("loaded JS plugin `{id}` from {}", dir.display()),
            None,
        );
        // If the Worker runtime is already running it can pick the new
        // plugin up immediately, otherwise it will ask for the full set when
        // it reports ready.
        if self.runtime_ready() {
            self.emit_js_load(&id);
        }
        Ok(())
    }

    fn insert(&self, plugin: Arc<LoadedPlugin>) -> PluginResult<()> {
        let id = plugin.manifest.id.clone();
        let mut plugins = self.inner.plugins.write().unwrap_or_else(|e| e.into_inner());
        plugins.insert(id, plugin);
        drop(plugins);
        self.notify_plugins_changed();
        Ok(())
    }

    pub(crate) fn get(&self, plugin_id: &str) -> Option<Arc<LoadedPlugin>> {
        let plugins = self.inner.plugins.read().unwrap_or_else(|e| e.into_inner());
        plugins.get(plugin_id).cloned()
    }

    pub fn list(&self) -> Vec<PluginInfo> {
        let plugins = self.inner.plugins.read().unwrap_or_else(|e| e.into_inner());
        plugins.values().map(|p| p.info()).collect()
    }

    pub fn contains(&self, plugin_id: &str) -> bool {
        let plugins = self.inner.plugins.read().unwrap_or_else(|e| e.into_inner());
        plugins.contains_key(plugin_id)
    }

    // ------------------------------------------------------- plugin directory

    /// Directory the framework scans for JS plugins.
    ///
    /// Order: `PLUGIN_FRAMEWORK_DIR`, `<cwd>/plugins`, `<cwd>/../plugins`,
    /// `<cwd>/src-tauri/plugins`. The first existing one wins; when none exists
    /// `<cwd>/plugins` is created.
    pub fn resolve_plugin_dir(&self) -> PathBuf {
        if let Some(dir) = self.inner.plugin_dir.get() {
            return dir.clone();
        }
        let mut candidates = Vec::new();
        if let Ok(dir) = env::var(PLUGIN_DIR_ENV) {
            candidates.push(PathBuf::from(dir));
        }
        if let Ok(cwd) = env::current_dir() {
            candidates.push(cwd.join("plugins"));
            candidates.push(cwd.join("..").join("plugins"));
            candidates.push(cwd.join("src-tauri").join("plugins"));
        }
        for candidate in candidates.iter() {
            if candidate.is_dir() {
                return canonical(candidate);
            }
        }
        candidates.into_iter().next().unwrap_or_else(|| PathBuf::from("plugins"))
    }

    pub fn set_plugin_dir(&self, dir: impl Into<PathBuf>) {
        let _ = self.inner.plugin_dir.set(dir.into());
    }

    /// Resolves the directory, scans it and runs the `on_load` hooks.
    pub fn bootstrap(&self) -> PluginResult<PathBuf> {
        let dir = self.resolve_plugin_dir();
        if !dir.exists() {
            let _ = fs::create_dir_all(&dir);
        }
        self.set_plugin_dir(dir.clone());
        self.push_log(
            "framework",
            "info",
            format!("plugin directory: {}", dir.display()),
            None,
        );
        let count = self.load_plugin_dir(&dir)?;
        self.push_log(
            "framework",
            "info",
            format!("{count} JS plugin(s) discovered on disk"),
            None,
        );
        self.run_load_hooks();
        self.notify_plugins_changed();
        Ok(dir)
    }

    /// Re-scans the plugin directory; already registered plugins are reloaded
    /// from disk (this is the "runtime loading" story).
    pub fn rescan(&self) -> PluginResult<Vec<PluginInfo>> {
        let dir = self.resolve_plugin_dir();
        self.load_plugin_dir(&dir)?;
        self.run_load_hooks();
        Ok(self.list())
    }

    fn run_load_hooks(&self) {
        let plugins: Vec<(String, Arc<dyn RustPlugin>)> = {
            let guard = self.inner.plugins.read().unwrap_or_else(|e| e.into_inner());
            guard
                .values()
                .filter_map(|p| match &p.imp {
                    PluginImpl::Rust(rust) if p.state.lock().map(|s| s.0).unwrap_or(PluginState::Pending) != PluginState::Loaded => {
                        Some((p.manifest.id.clone(), Arc::clone(rust)))
                    }
                    _ => None,
                })
                .collect()
        };

        for (id, plugin) in plugins {
            let host = self.clone();
            self.spawn_task(Box::pin(async move {
                let ctx = PluginCtx::from_parts(host.clone(), id.as_str());
                let Some(entry) = host.get(&id) else { return };
                match plugin.on_load(&ctx).await {
                    Ok(()) => entry.set_state(PluginState::Loaded, None),
                    Err(error) => {
                        host.push_log(&id, "error", format!("on_load failed: {error}"), None);
                        entry.set_state(PluginState::Error, Some(error.to_string()));
                    }
                }
                host.notify_plugins_changed();
            }));
        }
    }

    // ------------------------------------------------------------- invoke

    /// Actively calls a method implemented by a plugin.
    ///
    /// * Rust plugins run in-process, in the Tauri async runtime.
    /// * JS plugins are forwarded to their Worker and awaited.
    pub async fn invoke(
        &self,
        caller: Option<&str>,
        plugin_id: &str,
        method: &str,
        params: Value,
    ) -> PluginResult<Value> {
        let plugin = self
            .get(plugin_id)
            .ok_or_else(|| PluginError::not_found(plugin_id))?;

        match &plugin.imp {
            PluginImpl::Rust(rust) => {
                let ctx = PluginCtx::from_parts(self.clone(), plugin_id);
                let result = rust.invoke(&ctx, method, params).await;
                if let Err(error) = &result {
                    self.push_log(plugin_id, "error", format!("{method}: {error}"), None);
                }
                result
            }
            PluginImpl::Js(_) => {
                if !self.runtime_ready() {
                    return Err(PluginError::runtime_unavailable());
                }
                let call_id = self.inner.call_seq.fetch_add(1, Ordering::Relaxed);
                let receiver = self.register_pending(call_id);
                self.emit_event(
                    EVENT_JS_INVOKE,
                    json!({
                        "pluginId": plugin_id,
                        "callId": call_id,
                        "method": method,
                        "params": params,
                        "caller": caller,
                    }),
                );
                self.arm_watchdog(call_id, plugin_id, method);
                receiver
                    .await
                    .unwrap_or_else(|| Err(PluginError::message("JS runtime dropped the call")))
            }
        }
    }

    fn register_pending(
        &self,
        call_id: u64,
    ) -> oneshot::Receiver<PluginResult<Value>> {
        let (sender, receiver) = oneshot::channel();
        let mut pending = self.inner.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.insert(call_id, sender);
        receiver
    }

    /// Called by the webview when it has an answer for [`EVENT_JS_INVOKE`].
    pub fn resolve_pending(&self, call_id: u64, result: PluginResult<Value>) -> bool {
        let sender = {
            let mut pending = self.inner.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.remove(&call_id)
        };
        match sender {
            Some(sender) => sender.send(result).is_ok(),
            None => false,
        }
    }

    fn arm_watchdog(&self, call_id: u64, plugin_id: &str, method: &str) {
        let ms = self.invoke_timeout_ms();
        if ms == 0 {
            return;
        }
        let host = self.clone();
        let plugin_id = plugin_id.to_string();
        let method = method.to_string();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(ms));
            let sender = {
                let mut pending = host.inner.pending.lock().unwrap_or_else(|e| e.into_inner());
                pending.remove(&call_id)
            };
            if let Some(sender) = sender {
                let error = PluginError::timeout(&plugin_id, &method, ms);
                host.push_log(&plugin_id, "error", error.to_string(), None);
                let _ = sender.send(Err(error));
            }
        });
    }

    // --------------------------------------------------------- capabilities

    /// Maps a capability name to the permission a JS plugin must declare.
    fn required_permission(capability: &str) -> Option<&'static str> {
        match capability {
            "log" => Some("log"),
            "kv.get" | "kv.set" | "kv.delete" | "kv.keys" => Some("kv"),
            "emit" => Some("emit"),
            "callPlugin" => Some("call"),
            _ => None,
        }
    }

    /// Checks a permission token (`log`, `kv`, `emit`, `call`) against the
    /// manifest of a plugin.
    ///
    /// Rust plugins run in-process and are trusted; JS plugins only get what
    /// their `manifest.json` asks for, and a plugin list with no `permissions`
    /// at all falls back to `log` only.
    pub fn check_permission(&self, plugin_id: &str, permission: &str) -> PluginResult<()> {
        let plugin = self
            .get(plugin_id)
            .ok_or_else(|| PluginError::not_found(plugin_id))?;
        if plugin.manifest.kind == PluginKind::Rust {
            return Ok(());
        }
        let allowed = plugin
            .manifest
            .permissions
            .iter()
            .any(|p| p == "*" || p == permission)
            || (plugin.manifest.permissions.is_empty() && permission == "log");
        if allowed {
            Ok(())
        } else {
            Err(PluginError::denied(plugin_id, permission))
        }
    }

    /// Checks a capability name as used on the JS side (`log`, `kv.set`,
    /// `emit`, `callPlugin`) by mapping it to its permission token first.
    pub fn check_capability(&self, plugin_id: &str, capability: &str) -> PluginResult<()> {
        let required = Self::required_permission(capability)
            .ok_or_else(|| PluginError::unknown_capability(capability))?;
        self.check_permission(plugin_id, required)
    }

    /// Executes a capability on behalf of a JS plugin.
    ///
    /// Everything the Worker can reach goes through here, which is what makes
    /// the Worker boundary meaningful: the plugin never gets filesystem, IPC or
    /// `invoke` access of its own.
    pub async fn handle_capability(
        &self,
        plugin_id: &str,
        capability: &str,
        args: Value,
    ) -> PluginResult<Value> {
        self.check_capability(plugin_id, capability)?;
        let ctx = PluginCtx::from_parts(self.clone(), plugin_id);
        match capability {
            "log" => {
                let level = args.get("level").and_then(Value::as_str).unwrap_or("info");
                let message = args
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let message = match args.get("data") {
                    Some(data) if !data.is_null() => format!("{message} {}", data),
                    _ => message,
                };
                ctx.log(level, message)?;
                Ok(Value::Null)
            }
            "kv.get" => {
                let key = arg_str(&args, "key")?;
                Ok(ctx.kv_get(&key)?.unwrap_or(Value::Null))
            }
            "kv.set" => {
                let key = arg_str(&args, "key")?;
                let value = args.get("value").cloned().unwrap_or(Value::Null);
                ctx.kv_set(&key, value)?;
                Ok(Value::Null)
            }
            "kv.delete" => {
                let key = arg_str(&args, "key")?;
                ctx.kv_delete(&key)?;
                Ok(Value::Null)
            }
            "kv.keys" => Ok(json!(ctx.kv_keys()?)),
            "emit" => {
                let event = arg_str(&args, "event")?;
                let payload = args.get("payload").cloned().unwrap_or(Value::Null);
                ctx.emit(&event, payload)?;
                Ok(Value::Null)
            }
            "callPlugin" => {
                let target = arg_str(&args, "pluginId")?;
                let method = arg_str(&args, "method")?;
                let params = args.get("params").cloned().unwrap_or(Value::Null);
                self.invoke(Some(plugin_id), &target, &method, params).await
            }
            other => Err(PluginError::unknown_capability(other)),
        }
    }

    // ------------------------------------------------------------ shared kv

    pub fn kv_get(&self, key: &str) -> Option<Value> {
        let kv = self.inner.kv.lock().unwrap_or_else(|e| e.into_inner());
        kv.get(key).cloned()
    }

    pub fn kv_set(&self, key: &str, value: Value) {
        let key = key.to_string();
        {
            let mut kv = self.inner.kv.lock().unwrap_or_else(|e| e.into_inner());
            kv.insert(key.clone(), value);
        }
        self.emit_event(
            "framework://kv",
            json!({ "key": key, "changed": true }),
        );
    }

    pub fn kv_delete(&self, key: &str) {
        let removed = {
            let mut kv = self.inner.kv.lock().unwrap_or_else(|e| e.into_inner());
            kv.remove(key).is_some()
        };
        if removed {
            self.emit_event("framework://kv", json!({ "key": key, "changed": true }));
        }
    }

    pub fn kv_keys(&self) -> Vec<String> {
        let kv = self.inner.kv.lock().unwrap_or_else(|e| e.into_inner());
        let mut keys: Vec<String> = kv.keys().cloned().collect();
        keys.sort();
        keys
    }

    // -------------------------------------------------------- log & events

    pub fn push_log(
        &self,
        source: &str,
        level: &str,
        message: impl Into<String>,
        data: Option<Value>,
    ) {
        let entry = LogEntry {
            source: source.to_string(),
            level: level.to_string(),
            message: match data {
                Some(data) => format!("{} {data}", message.into()),
                None => message.into(),
            },
            timestamp: now_ms(),
        };
        eprintln!("[plugin:{source}] {level}: {}", entry.message);
        {
            let mut logs = self.inner.logs.lock().unwrap_or_else(|e| e.into_inner());
            if logs.len() >= RING_CAPACITY {
                logs.pop_front();
            }
            logs.push_back(entry.clone());
        }
        self.emit_event(EVENT_LOG, json!(entry));
    }

    pub fn logs(&self) -> Vec<LogEntry> {
        let logs = self.inner.logs.lock().unwrap_or_else(|e| e.into_inner());
        logs.iter().cloned().collect()
    }

    /// Broadcasts a plugin produced event.
    pub fn emit_plugin_event(&self, source: &str, event: &str, payload: Value) {
        let entry = FrameworkEvent {
            source: source.to_string(),
            event: event.to_string(),
            payload,
            timestamp: now_ms(),
        };
        {
            let mut events = self.inner.events.lock().unwrap_or_else(|e| e.into_inner());
            if events.len() >= RING_CAPACITY {
                events.pop_front();
            }
            events.push_back(entry.clone());
        }
        self.emit_event(EVENT_PLUGIN_EVENT, json!(entry));
    }

    pub fn events(&self) -> Vec<FrameworkEvent> {
        let events = self.inner.events.lock().unwrap_or_else(|e| e.into_inner());
        events.iter().cloned().collect()
    }

    // ---------------------------------------------------------- js runtime

    pub fn runtime_ready(&self) -> bool {
        self.inner.runtime_ready.load(Ordering::SeqCst)
    }

    /// Called by the webview once it is listening and can create Workers.
    /// Every known JS plugin is (re)loaded into its own Worker.
    pub fn mark_runtime_ready(&self) {
        self.inner.runtime_ready.store(true, Ordering::SeqCst);
        self.emit_event(EVENT_RUNTIME_STATUS, json!({ "ready": true }));
        let ids: Vec<String> = {
            let guard = self.inner.plugins.read().unwrap_or_else(|e| e.into_inner());
            guard
                .values()
                .filter(|p| p.manifest.kind == PluginKind::Js)
                .map(|p| p.manifest.id.clone())
                .collect()
        };
        for id in ids {
            self.emit_js_load(&id);
        }
        self.notify_plugins_changed();
    }

    /// Payloads for every JS plugin, used when the runtime (re)connects.
    pub fn js_plugin_payloads(&self) -> Vec<JsLoadPayload> {
        let guard = self.inner.plugins.read().unwrap_or_else(|e| e.into_inner());
        guard
            .values()
            .filter_map(|p| match &p.imp {
                PluginImpl::Js(js) => Some(JsLoadPayload {
                    id: p.manifest.id.clone(),
                    manifest: p.manifest.clone(),
                    source: js.source.clone(),
                    entry_path: js.entry_path.display().to_string(),
                }),
                _ => None,
            })
            .collect()
    }

    fn emit_js_load(&self, plugin_id: &str) {
        let payload = {
            let guard = self.inner.plugins.read().unwrap_or_else(|e| e.into_inner());
            guard.get(plugin_id).and_then(|p| match &p.imp {
                PluginImpl::Js(js) => Some(JsLoadPayload {
                    id: p.manifest.id.clone(),
                    manifest: p.manifest.clone(),
                    source: js.source.clone(),
                    entry_path: js.entry_path.display().to_string(),
                }),
                _ => None,
            })
        };
        if let Some(payload) = payload {
            self.emit_event(EVENT_JS_LOAD, json!(payload));
        }
    }

    /// The JS runtime reports that a Worker finished booting.
    pub fn mark_js_loaded(&self, plugin_id: &str) {
        if let Some(plugin) = self.get(plugin_id) {
            plugin.set_state(PluginState::Loaded, None);
        }
        self.notify_plugins_changed();
    }

    /// The JS runtime reports that a Worker could not boot.
    pub fn mark_js_failed(&self, plugin_id: &str, error: String) {
        self.push_log(plugin_id, "error", format!("worker failed: {error}"), None);
        if let Some(plugin) = self.get(plugin_id) {
            plugin.set_state(PluginState::Error, Some(error));
        }
        self.notify_plugins_changed();
    }

    // ------------------------------------------------------------- plumbing

    fn notify_plugins_changed(&self) {
        self.emit_event(EVENT_PLUGINS_CHANGED, json!({ "plugins": self.list() }));
    }

    fn emit_event(&self, event: &str, payload: Value) {
        self.bridge().emit(event, payload);
    }
}

fn arg_str(args: &Value, key: &str) -> PluginResult<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| PluginError::invalid_argument(format!("`{key}` must be a string")))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
