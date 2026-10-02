//! The trait implemented by plugins that live inside the Rust binary.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use crate::ctx::PluginCtx;
use crate::error::PluginResult;
use crate::manifest::PluginManifest;

/// Hand-rolled `BoxFuture` so the framework does not need `async-trait`
/// (or any other dependency) to be usable.
pub type PluginFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A plugin implemented in Rust, usually as part of the application (or of
/// another crate it ships).
///
/// Implementations are registered with
/// [`PluginHost::register_rust_plugin`](crate::PluginHost::register_rust_plugin)
/// and then become indistinguishable from JS plugins for callers: both are
/// addressed by id through [`PluginHost::invoke`](crate::PluginHost::invoke).
pub trait RustPlugin: Send + Sync + 'static {
    /// Static description of the plugin. Called on every registry lookup, so
    /// implementations should return a cached value.
    fn manifest(&self) -> &PluginManifest;

    /// Called once, right after the registry is built.
    fn on_load<'a>(&'a self, _ctx: &'a PluginCtx) -> PluginFuture<'a, PluginResult<()>> {
        Box::pin(async { Ok(()) })
    }

    /// Called when the framework is shutting down or the plugin is unloaded.
    fn on_unload<'a>(&'a self, _ctx: &'a PluginCtx) -> PluginFuture<'a, PluginResult<()>> {
        Box::pin(async { Ok(()) })
    }

    /// Entry point used by the framework to actively call the plugin.
    fn invoke<'a>(
        &'a self,
        ctx: &'a PluginCtx,
        method: &'a str,
        params: Value,
    ) -> PluginFuture<'a, PluginResult<Value>>;
}
