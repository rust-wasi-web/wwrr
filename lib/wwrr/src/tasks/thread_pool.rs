use std::sync::{Arc, OnceLock};
use std::{fmt::Debug, future::Future, pin::Pin};

use futures::future::LocalBoxFuture;
use futures::FutureExt;
use instant::Duration;
use virtual_mio::InlineSleep;
use wasmer_wasix::runtime::task_manager::SchedulerSpawn;
use wasmer_wasix::{runtime::task_manager::TaskWasm, VirtualTaskManager, WasiThreadError};

use super::scheduler::Scheduler;
use super::scheduler_message::SchedulerMsg;

/// A handle to a threadpool backed by Web Workers.
#[derive(Debug, Clone)]
pub struct ThreadPool {
    // Using wrappers that potentially block is safe here because
    // the initialization can only take place when only the main
    // thread is running.
    scheduler: Arc<OnceLock<Scheduler>>,
}

const CROSS_ORIGIN_WARNING: &str =
    r#"You can only run packages from "Cross-Origin Isolated" websites."#;

impl ThreadPool {
    pub fn new() -> Self {
        if let Some(cross_origin_isolated) = utils::GlobalScope::current().cross_origin_isolated() {
            // Note: This will need to be tweaked when we add support for Deno and
            // NodeJS.
            web_sys::console::assert_with_condition_and_data_1(
                cross_origin_isolated,
                &wasm_bindgen::JsValue::from_str(CROSS_ORIGIN_WARNING),
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
}

#[async_trait::async_trait]
impl VirtualTaskManager for ThreadPool {
    fn init(&self, scheduler_spawn: SchedulerSpawn) -> LocalBoxFuture<()> {
        async move {
            tracing::info!(
                "initializing thread pool with {} prestarted workers",
                scheduler_spawn.prestarted_workers
            );
            let scheduler = Scheduler::spawn(scheduler_spawn);
            scheduler.ping().await.unwrap();
            self.scheduler
                .set(scheduler)
                .expect("thread pool already initialized");
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
        let msg = crate::tasks::task_wasm::to_scheduler_message(task)?;
        self.send(msg);
        Ok(())
    }

    /// Returns the amount of parallelism that is possible on this platform
    fn thread_parallelism(&self) -> Result<usize, WasiThreadError> {
        match utils::GlobalScope::current().hardware_concurrency() {
            Some(n) => Ok(n.get()),
            None => Err(WasiThreadError::Unsupported),
        }
    }
}
