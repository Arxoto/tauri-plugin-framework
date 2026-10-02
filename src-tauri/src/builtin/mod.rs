//! Plugins that ship inside the binary: proof that a plugin does not need to
//! live on disk, and that the framework treats both kinds identically.

pub mod counter;
pub mod echo;

use std::sync::Arc;

use plugin_framework::{PluginHost, PluginResult};

pub fn register(host: &PluginHost) -> PluginResult<()> {
    host.register_rust_plugin(Arc::new(echo::EchoPlugin::new()))?;
    host.register_rust_plugin(Arc::new(counter::CounterPlugin::new()))?;
    Ok(())
}
