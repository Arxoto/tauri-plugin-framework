//! A stateful built-in plugin: shows a Rust plugin using the framework
//! capabilities (shared kv + logging + events) through `PluginCtx`.

use plugin_framework::{
    PluginCtx, PluginError, PluginFuture, PluginManifest, PluginResult, RustPlugin,
};
use serde_json::{json, Value};

const KEY: &str = "builtin.counter.value";

pub struct CounterPlugin {
    manifest: PluginManifest,
}

impl CounterPlugin {
    pub fn new() -> Self {
        Self {
            manifest: PluginManifest::rust("builtin.counter", "Counter (built-in Rust)")
                .with_version("0.1.0")
                .with_author("framework")
                .with_description("Stateful counter kept in the framework shared kv store.")
                .with_methods(["describe", "inc", "get", "reset", "fanout"]),
        }
    }

    fn read(ctx: &PluginCtx) -> i64 {
        ctx.kv_get(KEY)
            .ok()
            .flatten()
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
    }
}

impl Default for CounterPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl RustPlugin for CounterPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn on_load<'a>(&'a self, ctx: &'a PluginCtx) -> PluginFuture<'a, PluginResult<()>> {
        Box::pin(async move {
            ctx.kv_set(KEY, json!(0))?;
            ctx.info("builtin.counter ready, counter reset to 0")
        })
    }

    fn on_unload<'a>(&'a self, ctx: &'a PluginCtx) -> PluginFuture<'a, PluginResult<()>> {
        Box::pin(async move {
            let value = Self::read(ctx);
            ctx.warn(format!("unloading with final value {value}"))
        })
    }

    fn invoke<'a>(
        &'a self,
        ctx: &'a PluginCtx,
        method: &'a str,
        params: Value,
    ) -> PluginFuture<'a, PluginResult<Value>> {
        Box::pin(async move {
            match method {
                "describe" => Ok(json!({
                    "id": ctx.plugin_id(),
                    "kind": "rust",
                    "runtime": "in-process",
                    "storage": "framework kv",
                    "methods": self.manifest.methods,
                })),
                "inc" => {
                    let by = params.get("by").and_then(Value::as_i64).unwrap_or(1);
                    let next = Self::read(ctx) + by;
                    ctx.kv_set(KEY, json!(next))?;
                    ctx.info(format!("counter is now {next}"))?;
                    ctx.emit("builtin.counter.changed", json!({ "value": next, "by": by }))?;
                    Ok(json!({ "value": next }))
                }
                "get" => Ok(json!({ "value": Self::read(ctx) })),
                "reset" => {
                    ctx.kv_set(KEY, json!(0))?;
                    Ok(json!({ "value": 0 }))
                }
                // Cross-plugin call in the other direction: Rust -> JS.
                "fanout" => {
                    let text = params
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let counted = ctx
                        .call_plugin("demo.wordcount", "count", json!({ "text": text }))
                        .await?;
                    Ok(json!({
                        "counter": Self::read(ctx),
                        "wordcount": counted,
                    }))
                }
                other => Err(PluginError::unknown_method(&self.manifest.id, other)),
            }
        })
    }
}
