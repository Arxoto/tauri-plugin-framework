//! Minimal dependency-free oneshot channel.
//!
//! Used to bridge "a JS worker answered a call" back into the async task that
//! is awaiting the answer. The receiver resolves to `None` when the sender is
//! dropped without sending, so an awaiting task can never hang forever.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

struct Slot<T> {
    value: Option<T>,
    closed: bool,
    waker: Option<Waker>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self {
            value: None,
            closed: false,
            waker: None,
        }
    }
}

fn lock<T>(slot: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // A panic while holding this lock can only come from the polling side, and
    // the payload never contains a poisoned invariant, so recovering is fine.
    slot.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Sender<T> {
    slot: Arc<Mutex<Slot<T>>>,
}

pub struct Receiver<T> {
    slot: Arc<Mutex<Slot<T>>>,
}

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let slot = Arc::new(Mutex::new(Slot::default()));
    (
        Sender {
            slot: Arc::clone(&slot),
        },
        Receiver { slot: Arc::clone(&slot) },
    )
}

impl<T> Sender<T> {
    /// Completes the channel. Returns the payload back if it was already completed.
    pub fn send(self, value: T) -> Result<(), T> {
        {
            let mut slot = lock(&self.slot);
            if slot.value.is_some() {
                return Err(value);
            }
            slot.value = Some(value);
            if let Some(waker) = slot.waker.take() {
                waker.wake();
            }
        }
        Ok(())
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut slot = lock(&self.slot);
        slot.closed = true;
        if let Some(waker) = slot.waker.take() {
            waker.wake();
        }
    }
}

impl<T> Future for Receiver<T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut slot = lock(&self.slot);
        if let Some(value) = slot.value.take() {
            return Poll::Ready(Some(value));
        }
        if slot.closed {
            return Poll::Ready(None);
        }
        slot.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::channel;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, Wake, Waker};

    struct Noop;
    impl Wake for Noop {
        fn wake(self: std::sync::Arc<Self>) {}
    }

    fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
        let waker = Waker::from(std::sync::Arc::new(Noop));
        let mut cx = Context::from_waker(&waker);
        future.poll(&mut cx)
    }

    #[test]
    fn receives_sent_value() {
        let (tx, rx) = channel::<u32>();
        tx.send(7).unwrap();
        let mut fut = Box::pin(rx);
        assert!(matches!(poll_once(fut.as_mut()), Poll::Ready(Some(7))));
    }

    #[test]
    fn resolves_none_when_sender_dropped() {
        let (tx, rx) = channel::<u32>();
        drop(tx);
        let mut fut = Box::pin(rx);
        assert!(matches!(poll_once(fut.as_mut()), Poll::Ready(None)));
    }
}
