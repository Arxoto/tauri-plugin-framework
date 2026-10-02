//! A stateless built-in plugin: pure functions over the parameters it receives.

use plugin_framework::{
    PluginCtx, PluginFuture, PluginManifest, PluginResult, RustPlugin,
};
use serde_json::{json, Value};

pub struct EchoPlugin {
    manifest: PluginManifest,
}

impl EchoPlugin {
    pub fn new() -> Self {
        Self {
            manifest: PluginManifest::rust("builtin.echo", "Echo (built-in Rust)")
                .with_version("0.1.0")
                .with_author("framework")
                .with_description("Stateless helpers used to exercise the invoke path.")
                .with_methods(["describe", "echo", "reverse", "sum", "boom"]),
        }
    }
}

impl Default for EchoPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl RustPlugin for EchoPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn on_load<'a>(&'a self, ctx: &'a PluginCtx) -> PluginFuture<'a, PluginResult<()>> {
        Box::pin(async move { ctx.info("builtin.echo ready (in-process, no worker)") })
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
                    "methods": self.manifest.methods,
                })),
                "echo" => {
                    ctx.debug(format!("echo: {params}"))?;
                    Ok(params)
                }
                "reverse" => {
                    let text = params
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    Ok(json!({ "text": text.chars().rev().collect::<String>() }))
                }
                "sum" => {
                    let numbers = params
                        .get("numbers")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let sum: f64 = numbers.iter().filter_map(Value::as_f64).sum();
                    Ok(json!({ "sum": sum, "count": numbers.len() }))
                }
                "boom" => Err(plugin_framework::PluginError::message(
                    "builtin.echo.boom always fails on purpose",
                )),
                other => Err(plugin_framework::PluginError::unknown_method(
                    &self.manifest.id,
                    other,
                )),
            }
        })
    }
}
