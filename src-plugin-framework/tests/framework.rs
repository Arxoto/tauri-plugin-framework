//! Integration tests for the parts of the framework that do not need a webview:
//! plugin discovery, the Rust plugin path, capability checks and call routing.

use std::sync::Arc;

use plugin_framework::{
    block_on, PluginCtx, PluginError, PluginFuture, PluginHost, PluginKind, PluginManifest,
    PluginResult, RustPlugin, UiBridge, EVENT_LOG, EVENT_PLUGINS_CHANGED,
};
use serde_json::{json, Value};
use std::sync::Mutex;

/// Path to the demo plugins that ship with the repository.
fn demo_plugins_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../plugins")
        .canonicalize()
        .expect("demo plugins directory exists")
}

struct Doubler {
    manifest: PluginManifest,
}

impl Doubler {
    fn new() -> Self {
        Self {
            manifest: PluginManifest::rust("test.doubler", "Doubler")
                .with_methods(["describe", "double"]),
        }
    }
}

impl RustPlugin for Doubler {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn invoke<'a>(
        &'a self,
        ctx: &'a PluginCtx,
        method: &'a str,
        params: Value,
    ) -> PluginFuture<'a, PluginResult<Value>> {
        Box::pin(async move {
            match method {
                "describe" => Ok(json!({ "id": ctx.plugin_id(), "kind": "rust" })),
                "double" => {
                    let n = params.get("n").and_then(Value::as_i64).unwrap_or(0);
                    // exercise a capability from inside a Rust plugin
                    ctx.kv_set("test.doubler.last", json!(n * 2))?;
                    ctx.emit("test.doubler.doubled", json!({ "value": n * 2 }))?;
                    Ok(json!({ "value": n * 2 }))
                }
                other => Err(PluginError::unknown_method(&self.manifest.id, other)),
            }
        })
    }
}

#[test]
fn rust_plugins_are_invokable_in_process() {
    let host = PluginHost::new();
    host.register_rust_plugin(Arc::new(Doubler::new())).unwrap();

    let doubled = block_on(host.invoke(
        None,
        "test.doubler",
        "double",
        json!({ "n": 21 }),
    ))
    .unwrap();

    assert_eq!(doubled["value"], 42);
    assert_eq!(host.kv_get("test.doubler.last"), Some(json!(42)));
    assert!(host
        .events()
        .iter()
        .any(|e| e.event == "test.doubler.doubled"));
}

#[test]
fn framework_can_call_a_method_it_does_not_know_in_advance() {
    let host = PluginHost::new();
    host.register_rust_plugin(Arc::new(Doubler::new())).unwrap();

    let described = block_on(host.invoke(
        None,
        "test.doubler",
        "describe",
        Value::Null,
    ))
    .unwrap();

    assert_eq!(described["id"], "test.doubler");
}

#[test]
fn missing_plugin_and_missing_method_are_distinguishable() {
    let host = PluginHost::new();
    host.register_rust_plugin(Arc::new(Doubler::new())).unwrap();

    let missing_plugin =
        block_on(host.invoke(None, "nope", "double", Value::Null)).unwrap_err();
    assert_eq!(missing_plugin.code, "plugin_not_found");

    let missing_method =
        block_on(host.invoke(None, "test.doubler", "nope", Value::Null))
            .unwrap_err();
    assert_eq!(missing_method.code, "unknown_method");
}

#[test]
fn js_plugins_are_discovered_from_the_plugin_directory() {
    let host = PluginHost::new();
    let count = host.load_plugin_dir(&demo_plugins_dir()).unwrap();

    assert_eq!(count, 2, "both demo plugins should be picked up");

    let plugins = host.list();
    let ids: Vec<&str> = plugins.iter().map(|p| p.id.as_str()).collect();
    assert!(ids.contains(&"demo.wordcount"));
    assert!(ids.contains(&"demo.caller"));

    let wordcount = plugins.iter().find(|p| p.id == "demo.wordcount").unwrap();
    assert_eq!(wordcount.kind, PluginKind::Js);
    assert_eq!(wordcount.runtime, "worker");
    assert_eq!(wordcount.entry, "index.js");
    assert!(wordcount.origin.ends_with("wordcount"));
    assert_eq!(wordcount.permissions, vec!["log", "kv", "emit"]);
}

#[test]
fn js_calls_fail_cleanly_before_the_worker_runtime_exists() {
    let host = PluginHost::new();
    host.load_plugin_dir(&demo_plugins_dir()).unwrap();

    let error =
        block_on(host.invoke(None, "demo.wordcount", "count", Value::Null))
            .unwrap_err();

    assert_eq!(error.code, "js_runtime_unavailable");
}

#[test]
fn only_declared_capabilities_reach_a_js_plugin() {
    let host = PluginHost::new();
    host.load_plugin_dir(&demo_plugins_dir()).unwrap();

    // declared in manifest.json
    assert!(host.check_capability("demo.wordcount", "log").is_ok());
    assert!(host.check_capability("demo.wordcount", "kv.set").is_ok());
    assert!(host.check_capability("demo.wordcount", "emit").is_ok());

    // not declared: wordcount may not call other plugins
    let denied = host.check_capability("demo.wordcount", "callPlugin").unwrap_err();
    assert_eq!(denied.code, "capability_denied");

    // the caller plugin does declare it
    assert!(host.check_capability("demo.caller", "callPlugin").is_ok());

    // unknown capabilities never reach a plugin
    let unknown = host.check_capability("demo.caller", "fs.read").unwrap_err();
    assert_eq!(unknown.code, "unknown_capability");
}

#[test]
fn js_capability_calls_are_served_by_the_framework() {
    let host = PluginHost::new();
    host.load_plugin_dir(&demo_plugins_dir()).unwrap();

    block_on(host.handle_capability(
        "demo.wordcount",
        "kv.set",
        json!({ "key": "demo.wordcount.history", "value": [{ "words": 3 }] }),
    ))
    .unwrap();

    let value = block_on(host.handle_capability(
        "demo.wordcount",
        "kv.get",
        json!({ "key": "demo.wordcount.history" }),
    ))
    .unwrap();
    assert_eq!(value, json!([{ "words": 3 }]));

    let denied = block_on(host.handle_capability(
        "demo.wordcount",
        "callPlugin",
        json!({ "pluginId": "builtin.counter", "method": "get" }),
    ))
    .unwrap_err();
    assert_eq!(denied.code, "capability_denied");
}

#[test]
fn a_js_plugin_cannot_ask_rust_to_run_an_unknown_capability() {
    let host = PluginHost::new();
    host.load_plugin_dir(&demo_plugins_dir()).unwrap();

    let error = block_on(host.handle_capability(
        "demo.caller",
        "process.spawn",
        json!({}),
    ))
    .unwrap_err();

    assert_eq!(error.code, "unknown_capability");
}

#[test]
fn malformed_manifests_are_rejected_without_killing_the_scan() {
    let root = temp_dir("bad-manifest");
    let plugin_dir = root.join("broken");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.json"),
        r#"{ "id": "broken", "kind": "js", "entry": "missing.js" }"#,
    )
    .unwrap();

    let host = PluginHost::new();
    let loaded = host.load_plugin_dir(&root).unwrap();

    assert_eq!(loaded, 0, "a plugin whose entry file is missing is skipped");
    assert!(host.list().is_empty());
    assert!(host
        .logs()
        .iter()
        .any(|entry| entry.level == "error" && entry.message.contains("missing.js")));

    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn rust_only_plugins_cannot_be_shipped_on_disk() {
    let root = temp_dir("rust-on-disk");
    let plugin_dir = root.join("pretender");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.json"),
        r#"{ "id": "pretender", "kind": "rust", "entry": "index.js" }"#,
    )
    .unwrap();
    std::fs::write(plugin_dir.join("index.js"), "// nothing").unwrap();

    let host = PluginHost::new();
    assert_eq!(host.load_plugin_dir(&root).unwrap(), 0);

    std::fs::remove_dir_all(&root).unwrap();
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("plugin-framework-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Regression test: plugins are registered (and registry events emitted) before
/// the host application installs its real bridge. That early traffic must not
/// permanently bind the framework to the no-op bridge.
#[test]
fn a_bridge_installed_later_still_receives_events() {
    let host = PluginHost::new();
    // Emits `framework://plugins` while no bridge is installed yet.
    host.register_rust_plugin(Arc::new(Doubler::new())).unwrap();

    #[derive(Default)]
    struct Recorder {
        events: Mutex<Vec<String>>,
    }

    impl UiBridge for Recorder {
        fn emit(&self, event: &str, _payload: Value) {
            self.events.lock().unwrap().push(event.to_string());
        }
    }

    let recorder = Arc::new(Recorder::default());
    host.set_bridge(recorder.clone());

    // Anything emitted from now on must reach the recorder.
    host.load_plugin_dir(&demo_plugins_dir()).unwrap();
    host.emit_plugin_event("test", "test.ping", json!({ "n": 1 }));

    let events = recorder.events.lock().unwrap().clone();
    assert!(
        events.iter().any(|event| event == EVENT_PLUGINS_CHANGED),
        "registry changes should reach the bridge, saw {events:?}"
    );
    assert!(
        events.iter().any(|event| event == EVENT_LOG),
        "logs should reach the bridge, saw {events:?}"
    );
    assert!(events.iter().any(|event| event == "framework://event"));
}
