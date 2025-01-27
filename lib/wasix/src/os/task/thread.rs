use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, Condvar, Mutex, Weak},
    task::Waker,
};
use wasm_bindgen::{JsCast, JsValue};

use wasmer::{ExportError, InstantiationError, MemoryError};
use wasmer_wasix_types::{
    types::Signal,
    wasi::{Errno, ExitCode},
    wasix::ThreadStartType,
};

use crate::{
    os::task::process::{WasiProcessId, WasiProcessInner},
    WasiRuntimeError,
};

use super::{
    control_plane::TaskCountGuard,
    task_join_handle::{OwnedTaskStatus, TaskJoinHandle},
};

/// Represents the ID of a WASI thread
#[derive(Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WasiThreadId(u32);

impl WasiThreadId {
    pub fn raw(&self) -> u32 {
        self.0
    }

    pub fn inc(&mut self) -> WasiThreadId {
        let ret = *self;
        self.0 += 1;
        ret
    }
}

impl From<i32> for WasiThreadId {
    fn from(id: i32) -> Self {
        Self(id as u32)
    }
}

impl From<WasiThreadId> for i32 {
    fn from(val: WasiThreadId) -> Self {
        val.0 as i32
    }
}

impl From<u32> for WasiThreadId {
    fn from(id: u32) -> Self {
        Self(id)
    }
}

impl From<WasiThreadId> for u32 {
    fn from(t: WasiThreadId) -> u32 {
        t.0
    }
}

impl std::fmt::Display for WasiThreadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::fmt::Debug for WasiThreadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Represents a running thread which allows a joiner to
/// wait for the thread to exit
#[derive(Clone, Debug)]
pub struct WasiThread {
    state: Arc<WasiThreadState>,
    start: ThreadStartType,
}

impl WasiThread {
    pub fn id(&self) -> WasiThreadId {
        self.state.id
    }

    /// Gets the thread start type for this thread
    pub fn thread_start_type(&self) -> ThreadStartType {
        self.start
    }
}

/// A guard that ensures a thread is marked as terminated when dropped.
///
/// Normally the thread result should be manually registered with
/// [`WasiThread::set_status_running`] or [`WasiThread::set_status_finished`],
/// but this guard can ensure that the thread is marked as terminated even if
/// this is forgotten or a panic occurs.
pub struct WasiThreadRunGuard {
    pub thread: WasiThread,
}

impl WasiThreadRunGuard {
    pub fn new(thread: WasiThread) -> Self {
        Self { thread }
    }
}

impl Drop for WasiThreadRunGuard {
    fn drop(&mut self) {
        self.thread
            .set_status_finished(Err(
                crate::RuntimeError::new("Thread manager disconnected").into()
            ));
    }
}

#[derive(Debug)]
struct WasiThreadState {
    is_main: bool,
    pid: WasiProcessId,
    id: WasiThreadId,
    signals: Mutex<(Vec<Signal>, Vec<Waker>)>,
    status: Arc<OwnedTaskStatus>,

    // Registers the task termination with the ControlPlane on drop.
    // Never accessed, since it's a drop guard.
    _task_count_guard: TaskCountGuard,
}

impl WasiThread {
    pub fn new(
        pid: WasiProcessId,
        id: WasiThreadId,
        is_main: bool,
        status: Arc<OwnedTaskStatus>,
        guard: TaskCountGuard,
        start: ThreadStartType,
    ) -> Self {
        Self {
            state: Arc::new(WasiThreadState {
                is_main,
                pid,
                id,
                status,
                signals: Mutex::new((Vec::new(), Vec::new())),
                _task_count_guard: guard,
            }),
            start,
        }
    }

    /// Returns the process ID
    pub fn pid(&self) -> WasiProcessId {
        self.state.pid
    }

    /// Returns the thread ID
    pub fn tid(&self) -> WasiThreadId {
        self.state.id
    }

    /// Returns true if this thread is the main thread
    pub fn is_main(&self) -> bool {
        self.state.is_main
    }

    /// Get a join handle to watch the task status.
    pub fn join_handle(&self) -> TaskJoinHandle {
        self.state.status.handle()
    }

    // TODO: this should be private, access should go through utility methods.
    pub fn signals(&self) -> &Mutex<(Vec<Signal>, Vec<Waker>)> {
        &self.state.signals
    }

    pub fn set_status_running(&self) {
        self.state.status.set_running();
    }

    /// Gets or sets the exit code based of a signal that was received
    /// Note: if the exit code was already set earlier this method will
    /// just return that earlier set exit code
    pub fn set_or_get_exit_code_for_signal(&self, sig: Signal) -> ExitCode {
        let default_exitcode: ExitCode = match sig {
            Signal::Sigquit | Signal::Sigabrt => Errno::Success.into(),
            _ => Errno::Intr.into(),
        };
        // This will only set the status code if its not already set
        self.set_status_finished(Ok(default_exitcode));
        self.try_join()
            .map(|r| r.unwrap_or(default_exitcode))
            .unwrap_or(default_exitcode)
    }

    /// Marks the thread as finished (which will cause anyone that
    /// joined on it to wake up)
    pub fn set_status_finished(&self, res: Result<ExitCode, WasiRuntimeError>) {
        self.state.status.set_finished(res.map_err(Arc::new));
    }

    /// Waits until the thread is finished or the timeout is reached
    pub async fn join(&self) -> Result<ExitCode, Arc<WasiRuntimeError>> {
        self.state.status.await_termination().await
    }

    /// Attempts to join on the thread
    pub fn try_join(&self) -> Option<Result<ExitCode, Arc<WasiRuntimeError>>> {
        self.state.status.status().into_finished()
    }

    /// Adds a signal for this thread to process
    pub fn signal(&self, signal: Signal) {
        let tid = self.tid();
        tracing::trace!(%tid, "signal-thread({:?})", signal);

        let mut guard = self.state.signals.lock().unwrap();
        if !guard.0.contains(&signal) {
            guard.0.push(signal);
        }
        guard.1.drain(..).for_each(|w| w.wake());
    }

    /// Returns all the signals that are waiting to be processed
    pub fn has_signal(&self, signals: &[Signal]) -> bool {
        let guard = self.state.signals.lock().unwrap();
        for s in guard.0.iter() {
            if signals.contains(s) {
                return true;
            }
        }
        false
    }

    /// Waits for a signal to arrive
    pub async fn wait_for_signal(&self) {
        // This poller will process any signals when the main working function is idle
        struct SignalPoller<'a> {
            thread: &'a WasiThread,
        }
        impl<'a> std::future::Future for SignalPoller<'a> {
            type Output = ();
            fn poll(
                self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Self::Output> {
                if self.thread.has_signals_or_subscribe(cx.waker()) {
                    return std::task::Poll::Ready(());
                }
                std::task::Poll::Pending
            }
        }
        SignalPoller { thread: self }.await
    }

    /// Returns all the signals that are waiting to be processed
    pub fn pop_signals_or_subscribe(&self, waker: &Waker) -> Option<Vec<Signal>> {
        let mut guard = self.state.signals.lock().unwrap();
        let mut ret = Vec::new();
        std::mem::swap(&mut ret, &mut guard.0);
        match ret.is_empty() {
            true => {
                if !guard.1.iter().any(|w| w.will_wake(waker)) {
                    guard.1.push(waker.clone());
                }
                None
            }
            false => Some(ret),
        }
    }

    /// Returns all the signals that are waiting to be processed
    pub fn has_signals_or_subscribe(&self, waker: &Waker) -> bool {
        let mut guard = self.state.signals.lock().unwrap();
        let has_signals = !guard.0.is_empty();
        if !has_signals && !guard.1.iter().any(|w| w.will_wake(waker)) {
            guard.1.push(waker.clone());
        }
        has_signals
    }

    /// Returns all the signals that are waiting to be processed
    pub fn pop_signals(&self) -> Vec<Signal> {
        let mut guard = self.state.signals.lock().unwrap();
        let mut ret = Vec::new();
        std::mem::swap(&mut ret, &mut guard.0);
        ret
    }
}

#[derive(Debug)]
pub struct WasiThreadHandleProtected {
    thread: WasiThread,
    inner: Weak<(Mutex<WasiProcessInner>, Condvar)>,
}

#[derive(Debug, Clone)]
pub struct WasiThreadHandle {
    protected: Arc<WasiThreadHandleProtected>,
}

impl WasiThreadHandle {
    pub(crate) fn new(
        thread: WasiThread,
        inner: &Arc<(Mutex<WasiProcessInner>, Condvar)>,
    ) -> WasiThreadHandle {
        Self {
            protected: Arc::new(WasiThreadHandleProtected {
                thread,
                inner: Arc::downgrade(inner),
            }),
        }
    }

    pub fn id(&self) -> WasiThreadId {
        self.protected.thread.tid()
    }

    pub fn as_thread(&self) -> WasiThread {
        self.protected.thread.clone()
    }
}

impl Drop for WasiThreadHandleProtected {
    fn drop(&mut self) {
        let id = self.thread.tid();
        if let Some(inner) = Weak::upgrade(&self.inner) {
            let mut inner = inner.0.lock().unwrap();
            if let Some(ctrl) = inner.threads.remove(&id) {
                ctrl.set_status_finished(Ok(Errno::Success.into()));
            }
            inner.thread_count -= 1;
        }
    }
}

impl std::ops::Deref for WasiThreadHandle {
    type Target = WasiThread;

    fn deref(&self) -> &Self::Target {
        &self.protected.thread
    }
}

#[derive(thiserror::Error, Debug, Clone)]
pub enum WasiThreadError {
    #[error("Multithreading is not supported")]
    Unsupported,
    #[error("The method named is not an exported function")]
    MethodNotFound,
    #[error("Failed to create the requested memory - {0}")]
    MemoryCreateFailed(MemoryError),
    #[error("{0}")]
    ExportError(ExportError),
    #[error("Failed to create the instance")]
    // Note: Boxed so we can keep the error size down
    InstanceCreateFailed(Box<InstantiationError>),
    #[error("Initialization function failed - {0}")]
    InitFailed(Arc<anyhow::Error>),
    /// This will happen if WASM is running in a thread has not been created by the spawn_wasm call
    #[error("WASM context is invalid")]
    InvalidWasmContext,
    #[error("wasm-bindgen interaction failed - {0}")]
    WbgFailed(String),
}

impl From<WasiThreadError> for Errno {
    fn from(a: WasiThreadError) -> Errno {
        match a {
            WasiThreadError::Unsupported => Errno::Notsup,
            WasiThreadError::MethodNotFound => Errno::Inval,
            WasiThreadError::MemoryCreateFailed(_) => Errno::Nomem,
            WasiThreadError::ExportError(_) => Errno::Noexec,
            WasiThreadError::InstanceCreateFailed(_) => Errno::Noexec,
            WasiThreadError::InitFailed(_) => Errno::Noexec,
            WasiThreadError::InvalidWasmContext => Errno::Noexec,
            WasiThreadError::WbgFailed(_) => Errno::Noexec,
        }
    }
}

impl WasiThreadError {
    fn js_err_str(js: &JsValue) -> String {
        if let Some(e) = js.dyn_ref::<js_sys::Error>() {
            format!("{}", String::from(e.message()))
        } else if let Some(obj) = js.dyn_ref::<js_sys::Object>() {
            format!("{}", String::from(obj.to_string()))
        } else if let Some(s) = js.dyn_ref::<js_sys::JsString>() {
            format!("{}", String::from(s))
        } else {
            format!("A JavaScript error occurred")
        }
    }

    /// wasm-bindgen interaction failed.
    pub fn wbg_failed(err: JsValue) -> Self {
        Self::WbgFailed(Self::js_err_str(&err))
    }
}
