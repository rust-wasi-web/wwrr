use std::sync::{Arc, OnceLock};
use std::{fmt::Debug, future::Future, pin::Pin};

use futures::future::LocalBoxFuture;
use futures::FutureExt;
use instant::Duration;
use virtual_mio::InlineSleep;
use wasm_bindgen::JsCast;
use wasmer_wasix::runtime::task_manager::SchedulerSpawn;
use wasmer_wasix::{runtime::task_manager::TaskWasm, VirtualTaskManager, WasiThreadError};

use super::scheduler::Scheduler;
use super::scheduler_message::SchedulerMsg;
use crate::run::is_memory_shared;

/// A handle to a threadpool backed by Web Workers.
#[derive(Debug, Clone)]
pub struct ThreadPool {
    // Using wrappers that potentially block is safe here because
    // the initialization can only take place when only the main
    // thread is running.
    scheduler: Arc<OnceLock<Scheduler>>,
}

impl ThreadPool {
    pub fn new() -> Self {
        if Self::available() {
            // Check that our memmory is shared correctly.
            let our_memory = wasm_bindgen::memory();
            assert!(
                is_memory_shared(our_memory.dyn_ref().unwrap()),
                "WWRR memory is not shared"
            );
        }

        Self {
            scheduler: Arc::new(OnceLock::new()),
        }
    }

    pub(crate) fn send(&self, msg: SchedulerMsg) {
        let scheduler = self.scheduler.get().expect("thread pool not initialized");
        scheduler.send(msg).expect("scheduler is dead");
    }

    /// Whether threading is available.
    pub fn available() -> bool {
        utils::GlobalScope::current()
            .cross_origin_isolated()
            .unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl VirtualTaskManager for ThreadPool {
    fn init(&self, scheduler_spawn: SchedulerSpawn) -> LocalBoxFuture<()> {
        async move {
            if Self::available() {
                tracing::debug!(
                    "initializing thread pool with {} pre-started workers",
                    scheduler_spawn.prestarted_workers
                );
                let scheduler = Scheduler::spawn(scheduler_spawn);
                scheduler.ping().await.unwrap();
                self.scheduler
                    .set(scheduler)
                    .expect("thread pool already initialized");
            }
        }
        .boxed_local()
    }

    fn sleep_now(
        &self,
        time: Duration,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>> {
        Box::pin(InlineSleep::new(time))
    }

    /// Starts an asynchronous task will will run on a dedicated thread
    /// pulled from the worker pool that has a stateful thread local variable
    /// It is ok for this task to block execution and any async futures within its scope
    fn task_wasm(&self, task: TaskWasm<'_, '_>) -> Result<(), WasiThreadError> {
        if !Self::available() {
            tracing::warn!(
                "Multi-threading is disabled because the site is not \
                 properly cross-origin isolated."
            );
            return Err(WasiThreadError::Unsupported);
        }

        let msg = crate::tasks::task_wasm::to_scheduler_message(task)?;
        self.send(msg);
        Ok(())
    }

    /// Returns the amount of parallelism that is possible on this platform
    fn thread_parallelism(&self) -> Result<usize, WasiThreadError> {
        if !Self::available() {
            return Ok(1);
        }

        match utils::GlobalScope::current().hardware_concurrency() {
            Some(n) => Ok(n.get()),
            None => Err(WasiThreadError::Unsupported),
        }
    }
}
