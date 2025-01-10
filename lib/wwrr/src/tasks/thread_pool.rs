use std::sync::OnceLock;
use std::{fmt::Debug, future::Future, pin::Pin};

use futures::future::LocalBoxFuture;
use futures::FutureExt;
use instant::Duration;
use virtual_mio::InlineSleep;
use wasmer::{Memory, Module};
use wasmer_wasix::{runtime::task_manager::TaskWasm, VirtualTaskManager, WasiThreadError};

use super::scheduler::Scheduler;
use super::scheduler_message::SchedulerMsg;

/// A handle to a threadpool backed by Web Workers.
#[derive(Debug, Clone)]
pub struct ThreadPool(OnceLock<Scheduler>);

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

        Self(OnceLock::new())
    }

    pub(crate) fn send(&self, msg: SchedulerMsg) {
        let scheduler = self.0.get().expect("thread pool not initialized");
        scheduler.send(msg).expect("scheduler is dead");
    }
}

#[async_trait::async_trait]
impl VirtualTaskManager for ThreadPool {
    fn init(
        &self,
        module: Module,
        memory: Memory,
        wbg_js_module_name: String,
    ) -> LocalBoxFuture<()> {
        async move {
            let scheduler = Scheduler::spawn(module, memory, wbg_js_module_name);
            scheduler.ping().await.unwrap();
            self.0.set(scheduler).unwrap();
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
