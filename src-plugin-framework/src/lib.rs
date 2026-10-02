//! # plugin-framework
//!
//! A small runtime plugin framework for desktop apps (the demo host is Tauri).
//! It exists to answer one question: *can an application load plugins at
//! runtime, with plugins written either in Rust (compiled in) or in JS (shipped
//! as a directory), and have the framework actively call into them?*
//!
//! The answer this crate demonstrates is yes, with two rules:
//!
//! 1. **Rust plugins** implement [`RustPlugin`] and are registered with
//!    [`PluginHost::register_rust_plugin`]. They run in-process and talk to the
//!    framework through a [`PluginCtx`].
//! 2. **JS plugins** live in a directory as `manifest.json` + `index.js`. The
//!    framework reads them at runtime, hands the source to the webview, and the
//!    webview runs each plugin inside its **own Web Worker**. The Worker has no
//!    DOM, no Tauri IPC and no filesystem: every capability is an RPC through
//!    the `ctx` shim and is checked against the manifest permissions.
//!
//! Both kinds are addressed identically by id:
//!
//! ```ignore
//! let value = host.invoke(None, "demo.wordcount", "count", json!({ "text": "a b" })).await?;
//! ```
//!
//! The core has **no dependency on Tauri** (or on any GUI toolkit). It reaches
//! the host through two traits, implemented by the application:
//!
//! * [`UiBridge`] to deliver events (logs, plugin events, Worker instructions),
//! * [`TaskSpawner`] to drive background work such as `on_load` hooks.
//!
//! See `docs/architecture.md` in the repository for the full picture.

mod bridge;
mod ctx;
mod error;
mod host;
mod manifest;
mod oneshot;
mod rust_plugin;

pub use bridge::{block_on, BoxFuture, NullBridge, TaskSpawner, ThreadSpawner, UiBridge};
pub use ctx::{level, PluginCtx};
pub use error::{PluginError, PluginResult};
pub use host::{
    FrameworkEvent, JsLoadPayload, LogEntry, PluginHost, PluginInfo, PluginState,
    EVENT_JS_INVOKE, EVENT_JS_LOAD, EVENT_JS_UNLOAD, EVENT_LOG, EVENT_PLUGINS_CHANGED,
    EVENT_PLUGIN_EVENT, EVENT_RUNTIME_STATUS, PLUGIN_DIR_ENV,
};
pub use manifest::{PluginKind, PluginManifest, DEFAULT_ENTRY, DEFAULT_RUNTIME, MANIFEST_FILE};
pub use rust_plugin::{PluginFuture, RustPlugin};

/// Version of the framework itself.
pub const FRAMEWORK_VERSION: &str = env!("CARGO_PKG_VERSION");
