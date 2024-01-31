//! Golang like WaitGroup implementation that supports both sync and async Rust.

#![deny(missing_docs)]
#![deny(unsafe_code)]
#![deny(unused_qualifications)]

extern crate alloc;

use alloc::sync::Arc;
use core::pin::Pin;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::task::Poll;
#[cfg(feature = "std")]
use std::fmt;

use event_listener::{Event, EventListener};
use event_listener_strategy::{easy_wrapper, EventListenerFuture, Strategy};
use futures_core::ready;

/// Enables tasks to synchronize the beginning or end of some computation.
///
/// # Examples
///
/// ```
/// use async_waitgroup::WaitGroup;
///
/// # #[tokio::main(flavor = "current_thread")] async fn main() {
/// // Create a new wait group.
/// let wg = WaitGroup::new();
///
/// for _ in 0..4 {
///     // Create another reference to the wait group.
///     let wg = wg.clone();
///
///     tokio::spawn(async move {
///         // Do some work.
///
///         // Drop the reference to the wait group.
///         drop(wg);
///     });
/// }
///
/// // Block until all tasks have finished their work.
/// wg.wait().await;
/// # }
/// ```
pub struct WaitGroup {
    inner: Arc<WgInner>,
}

/// Inner state of a `WaitGroup`.
struct WgInner {
    count: AtomicUsize,
    drop_ops: Event,
}

impl Default for WaitGroup {
    fn default() -> Self {
        Self {
            inner: Arc::new(WgInner {
                count: AtomicUsize::new(1),
                drop_ops: Event::new(),
            }),
        }
    }
}

impl WaitGroup {
    /// Creates a new wait group and returns the single reference to it.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_waitgroup::WaitGroup;
    ///
    /// let wg = WaitGroup::new();
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Drops this reference and waits until all other references are dropped.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_waitgroup::WaitGroup;
    ///
    /// # #[tokio::main(flavor = "current_thread")] async fn main() {
    /// let wg = WaitGroup::new();
    ///
    /// tokio::spawn({
    ///     let wg = wg.clone();
    ///     async move {
    ///         // Block until both tasks have reached `wait()`.
    ///         wg.wait().await;
    ///     }
    /// });
    ///
    /// // Block until all tasks have finished their work.
    /// wg.wait().await;
    /// # }
    /// ```
    pub fn wait(self) -> Wait {
        Wait::_new(WaitInner {
            wg: self.inner.clone(),
            listener: EventListener::new(),
        })
    }

    /// Waits using the blocking strategy.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::thread;
    ///
    /// use async_waitgroup::WaitGroup;
    ///
    /// let wg = WaitGroup::new();
    ///
    /// thread::spawn({
    ///     let wg = wg.clone();
    ///     move || {
    ///         wg.wait_blocking();
    ///     }
    /// });
    ///
    /// wg.wait_blocking();
    /// ```
    #[cfg(all(feature = "std", not(target_family = "wasm")))]
    pub fn wait_blocking(self) {
        self.wait().wait();
    }
}

easy_wrapper! {
    /// A future returned by [`WaitGroup::wait()`].
    #[must_use = "futures do nothing unless you `.await` or poll them"]
    pub struct Wait(WaitInner => ());
    #[cfg(all(feature = "std", not(target_family = "wasm")))]
    pub(crate) wait();
}

pin_project_lite::pin_project! {
    struct WaitInner {
        wg: Arc<WgInner>,
        #[pin]
        listener: EventListener,
    }
}

impl EventListenerFuture for WaitInner {
    type Output = ();

    fn poll_with_strategy<'a, S: Strategy<'a>>(
        self: Pin<&mut Self>,
        strategy: &mut S,
        context: &mut S::Context,
    ) -> Poll<Self::Output> {
        let mut this = self.project();

        if this.wg.count.load(Ordering::SeqCst) == 0 {
            return Poll::Ready(());
        }

        let mut count = this.wg.count.load(Ordering::SeqCst);
        while count > 0 {
            if this.listener.is_listening() {
                ready!(strategy.poll(this.listener.as_mut(), context))
            } else {
                this.listener.as_mut().listen(&this.wg.drop_ops);
            }
            count = this.wg.count.load(Ordering::SeqCst);
        }

        Poll::Ready(())
    }
}

impl Drop for WaitGroup {
    fn drop(&mut self) {
        if self.inner.count.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.inner.drop_ops.notify(usize::MAX);
        }
    }
}

impl Clone for WaitGroup {
    fn clone(&self) -> Self {
        self.inner.count.fetch_add(1, Ordering::SeqCst);

        Self {
            inner: self.inner.clone(),
        }
    }
}

#[cfg(feature = "std")]
impl fmt::Debug for WaitGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.inner.count.load(Ordering::SeqCst);
        f.debug_struct("WaitGroup").field("count", &count).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "std")]
    use std::thread;

    #[tokio::test]
    async fn test_wait() {
        const LOOP: usize = 10_000;

        let wg = WaitGroup::new();
        let cnt = Arc::new(AtomicUsize::new(0));

        for _ in 0..LOOP {
            tokio::spawn({
                let wg = wg.clone();
                let cnt = cnt.clone();
                async move {
                    cnt.fetch_add(1, Ordering::Relaxed);
                    drop(wg);
                }
            });
        }

        wg.wait().await;
        assert_eq!(cnt.load(Ordering::Relaxed), LOOP)
    }

    #[cfg(all(feature = "std", not(target_family = "wasm")))]
    #[test]
    fn test_wait_blocking() {
        const LOOP: usize = 100;

        let wg = WaitGroup::new();
        let cnt = Arc::new(AtomicUsize::new(0));

        for _ in 0..LOOP {
            thread::spawn({
                let wg = wg.clone();
                let cnt = cnt.clone();
                move || {
                    cnt.fetch_add(1, Ordering::Relaxed);
                    drop(wg);
                }
            });
        }

        wg.wait_blocking();
        assert_eq!(cnt.load(Ordering::Relaxed), LOOP)
    }

    #[test]
    fn test_clone() {
        let wg = WaitGroup::new();
        assert_eq!(Arc::strong_count(&wg.inner), 1);

        let wg2 = wg.clone();
        assert_eq!(Arc::strong_count(&wg.inner), 2);
        assert_eq!(Arc::strong_count(&wg2.inner), 2);
        drop(wg2);
        assert_eq!(Arc::strong_count(&wg.inner), 1);
    }

    #[tokio::test]
    async fn test_futures() {
        let wg = WaitGroup::new();
        let wg2 = wg.clone();

        let w = wg.wait();
        pin_utils::pin_mut!(w);
        assert_eq!(futures_util::poll!(w.as_mut()), Poll::Pending);
        assert_eq!(futures_util::poll!(w.as_mut()), Poll::Pending);

        drop(wg2);
        assert_eq!(futures_util::poll!(w.as_mut()), Poll::Ready(()));
    }
}
