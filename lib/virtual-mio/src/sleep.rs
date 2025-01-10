use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use utils::GlobalScope;

use crate::InlineWaker;

/// Sleeps for the specified amount of time.
///
/// Works together with [`InlineWaker`] to yield to the execution environment
/// when possible.
#[derive(Debug)]
pub struct InlineSleep {
    until: f64,
}

impl Future for InlineSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let this = self.as_ref();
        let now = GlobalScope::current().now();

        if now >= this.until {
            Poll::Ready(())
        } else {
            let waker = cx.waker();
            match <&InlineWaker>::try_from(waker) {
                Ok(inline_waker) => {
                    let remaining = this.until - now;
                    inline_waker.set_timeout(Duration::from_millis(remaining as _));
                }
                Err(_) => {
                    tracing::warn!(
                        "InlineSleep reverts to spin wait when not used together with InlineWaker"
                    );
                    waker.wake_by_ref();
                }
            }
            Poll::Pending
        }
    }
}

impl InlineSleep {
    /// Creates a future that sleeps for the specified amount of time.
    pub fn new(duration: Duration) -> Self {
        Self {
            until: GlobalScope::current().now() + duration.as_millis() as f64,
        }
    }
}
