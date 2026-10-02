//! The two seams between the framework core and the host application.
//!
//! The core deliberately knows nothing about Tauri: it only needs somebody to
//! deliver events to (the webview) and somebody to drive background tasks
//! (`on_load` hooks). Keeping those behind traits is what lets the registry be
//! tested headlessly, and lets the same crate be reused by another host.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::Duration;

use serde_json::Value;

/// A future the framework can hand to a [`TaskSpawner`].
pub type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Sink for everything the framework wants the UI to see: logs, plugin events,
/// registry changes and the Worker instructions for JS plugins.
pub trait UiBridge: Send + Sync + 'static {
    fn emit(&self, event: &str, payload: Value);
}

/// Default bridge, used by tests and by any host that does not care about
/// events. Everything still works, events are simply dropped.
#[derive(Debug, Default)]
pub struct NullBridge;

impl UiBridge for NullBridge {
    fn emit(&self, _event: &str, _payload: Value) {}
}

/// Drives background work started by the framework.
pub trait TaskSpawner: Send + Sync + 'static {
    fn spawn(&self, task: BoxFuture);
}

/// Default spawner: one thread per task running [`block_on`].
///
/// Plugin futures are waker driven (they only wait on framework callbacks such
/// as a Worker reply), so a tiny park/unpark executor is enough.
#[derive(Debug, Default)]
pub struct ThreadSpawner;

impl TaskSpawner for ThreadSpawner {
    fn spawn(&self, task: BoxFuture) {
        thread::spawn(move || block_on(task));
    }
}

struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Minimal executor used by [`ThreadSpawner`] and exposed for tests.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
        // The timeout is a safety net: even if a wakeup is ever missed, the
        // task still makes progress instead of parking forever.
        thread::park_timeout(Duration::from_millis(50));
    }
}
