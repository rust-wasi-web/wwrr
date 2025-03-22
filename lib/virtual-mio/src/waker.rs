use std::error::Error;
use std::ptr;
use std::time::Duration;
use std::{fmt, mem};
use std::{
    sync::{Arc, Condvar, Mutex},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

use futures::Future;
use utils::GlobalScope;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    type Atomics;

    #[wasm_bindgen(static_method_of = Atomics, js_name = pause)]
    fn pause();

    #[wasm_bindgen(static_method_of = Atomics, js_name = pause, getter)]
    fn get_pause() -> JsValue;
}

/// Calls Atomics.pause(), if available.
#[inline]
pub fn atomics_pause() {
    thread_local! {
        static HAS_PAUSE: bool = !Atomics::get_pause().is_undefined();
    }

    if HAS_PAUSE.with(|p| *p) {
        Atomics::pause();
    }
}

/// Runs a Future on the current thread.
pub struct InlineWaker {
    signature: [u8; 4],
    woken: Mutex<bool>,
    condvar: Condvar,
    timeout: Mutex<Option<Duration>>,
}

impl InlineWaker {
    const SIGNATURE: [u8; 4] = *b"IlWk";

    /// Create a new wake that runs the future on the current thread.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            signature: Self::SIGNATURE,
            woken: Mutex::new(false),
            condvar: Condvar::new(),
            timeout: Mutex::new(None),
        })
    }

    fn wake_now(&self) {
        {
            let mut woken = self.woken.lock().unwrap();
            *woken = true;
        }

        self.condvar.notify_all();
    }

    /// Convert to [`Waker`].
    pub fn as_waker(self: &Arc<Self>) -> Waker {
        let s: *const Self = Arc::into_raw(Arc::clone(self));
        let raw_waker = RawWaker::new(s as *const (), &VTABLE);
        unsafe { Waker::from_raw(raw_waker) }
    }

    /// Runs the specified Future blocking the current thread.
    pub fn block_on<'a, A>(task: impl Future<Output = A> + 'a) -> A {
        const SPIN_TIMEOUT_MS: f64 = 10_000.;

        // Create the waker
        let inline_waker = Self::new();
        let waker = inline_waker.as_waker();
        let mut cx = Context::from_waker(&waker);

        let mut task = Box::pin(task);

        let global = GlobalScope::current();

        if global.is_wait_allowed() {
            // We loop waiting for the waker to be woken, then we poll again.
            loop {
                match task.as_mut().poll(&mut cx) {
                    Poll::Ready(ret) => return ret,
                    Poll::Pending => {
                        let mut woken = inline_waker.woken.lock().unwrap();
                        let timeout = inline_waker.timeout.lock().unwrap().take();

                        if !*woken {
                            match timeout {
                                Some(timeout) => {
                                    woken = inline_waker
                                        .condvar
                                        .wait_timeout(woken, timeout)
                                        .unwrap()
                                        .0;
                                }
                                None => woken = inline_waker.condvar.wait(woken).unwrap(),
                            }
                        }

                        *woken = false;
                    }
                }
            }
        } else {
            // We spin, since wait operations are not allowed on this thread.
            let until = global.now() + SPIN_TIMEOUT_MS;

            while global.now() <= until {
                match task.as_mut().poll(&mut cx) {
                    Poll::Ready(ret) => return ret,
                    Poll::Pending => {
                        atomics_pause();
                        std::thread::yield_now();
                    }
                }
            }

            panic!("timeout while blocking main thread");
        }
    }

    /// Sets the timeout after which the waker wakes, even if not explictly woken.
    ///
    /// If multiple timeouts are set, the shortest timeout is used.
    pub fn set_timeout(&self, duration: Duration) {
        let mut timeout = self.timeout.lock().unwrap();
        match &mut *timeout {
            Some(timeout) => *timeout = (*timeout).min(duration),
            None => *timeout = Some(duration),
        }
    }

    /// Whether waiting is allowed on the current thread.
    pub fn is_wait_allowed(&self) -> bool {
        GlobalScope::current().is_wait_allowed()
    }
}

/// The provided waker is not an [`InlineWaker`].
#[derive(Clone, Debug)]
pub struct NotInlineWaker {}

impl fmt::Display for NotInlineWaker {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "the waker is not an InlineWaker")
    }
}

impl Error for NotInlineWaker {}

impl TryFrom<&Waker> for &InlineWaker {
    type Error = NotInlineWaker;

    fn try_from(waker: &Waker) -> Result<Self, Self::Error> {
        unsafe {
            let data = waker.data();

            // This is not 100% safe, since we might try to read past the end of memory,
            // but we accept that risk for now.
            let sig: [u8; 4] =
                ptr::read(data.byte_add(mem::offset_of!(InlineWaker, signature)) as _);
            if sig != InlineWaker::SIGNATURE {
                return Err(NotInlineWaker {});
            }

            Ok(&*(data as *const InlineWaker))
        }
    }
}

fn inline_waker_wake(s: &InlineWaker) {
    let waker_arc = unsafe { Arc::from_raw(s) };
    waker_arc.wake_now();
}

fn inline_waker_clone(s: &InlineWaker) -> RawWaker {
    let arc = unsafe { Arc::from_raw(s) };
    std::mem::forget(arc.clone());
    RawWaker::new(Arc::into_raw(arc) as *const (), &VTABLE)
}

const VTABLE: RawWakerVTable = unsafe {
    RawWakerVTable::new(
        |s| inline_waker_clone(&*(s as *const InlineWaker)), // clone
        |s| inline_waker_wake(&*(s as *const InlineWaker)),  // wake
        |s| (*(s as *const InlineWaker)).wake_now(), // wake by ref (don't decrease refcount)
        |s| drop(Arc::from_raw(s as *const InlineWaker)), // decrease refcount
    )
};
