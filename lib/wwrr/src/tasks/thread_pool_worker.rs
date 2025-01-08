use js_sys::Promise;
use tokio::sync::{mpsc, oneshot};
use wasm_bindgen::{prelude::wasm_bindgen, JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::DedicatedWorkerGlobalScope;

use super::scheduler::Scheduler;
use super::scheduler_message::SchedulerMsg;
use super::worker_message::WorkerMsg;
use crate::tasks::WorkerInit;

/// Worker initialization state.
#[wasm_bindgen(skip_typescript)]
#[derive(Debug)]
pub struct ThreadPoolWorker {
    /// Worker id.
    id: u32,
}

#[wasm_bindgen]
impl ThreadPoolWorker {
    /// Preinitializes the worker.
    #[wasm_bindgen(constructor)]
    pub fn new(id: u32) -> Self {
        Self { id }
    }

    /// Handles the init message and starts the worker.
    #[wasm_bindgen]
    pub fn handle(&self, msg: JsValue) -> Result<(), utils::Error> {
        tracing::info!("Initializing worker");
        let worker_init = unsafe { WorkerInit::try_from_js(msg) }?;
        ThreadPoolWorkerState::spawn(self.id, worker_init);
        Ok(())
    }
}

/// Worker state.
#[derive(Debug)]
pub struct ThreadPoolWorkerState {
    /// Worker id.
    id: u32,
    /// Scheduler.
    scheduler: Scheduler,
    /// Message receiver.
    msg_rx: mpsc::UnboundedReceiver<WorkerMsg>,
    /// WebAssembly module for spawning threads.
    module: wasmer::Module,
    /// WebAssembly memory for spawning threads.
    memory: wasmer::Memory,
}

impl ThreadPoolWorkerState {
    /// Run the worker on this web worker.
    pub fn spawn(id: u32, init: WorkerInit) {
        let WorkerInit {
            scheduler,
            ready_tx,
            msg_rx,
            module,
            memory,
            _not_send,
        } = init;

        let this = Self {
            id,
            scheduler,
            msg_rx,
            module,
            memory,
        };
        wasm_bindgen_futures::spawn_local(this.run(ready_tx));
    }

    /// Worker main loop.
    async fn run(mut self, ready_tx: oneshot::Sender<()>) {
        tracing::info!("worker {} running", self.id);

        ready_tx.send(()).unwrap();

        while let Some(msg) = self.msg_rx.recv().await {
            if let Err(e) = self.execute(msg).await {
                tracing::error!("An error occurred while handling a message: {e}");
            }
        }

        tracing::info!("worker exiting");
        let scope: DedicatedWorkerGlobalScope = js_sys::global().dyn_into().unwrap();
        scope.close();
    }

    #[tracing::instrument(level = "debug", skip_all, fields(worker.id = self.id))]
    async fn execute(&self, msg: WorkerMsg) -> Result<(), utils::Error> {
        tracing::trace!(?msg, "Handling a message");

        match msg {
            WorkerMsg::SpawnWasm(spawn_wasm) => {
                tracing::info!("spawn_wasm in worker");
                let _guard = self.busy();

                // Import wasm-bindgen generated code.
                let wbg_mod = match &spawn_wasm.env.wbg_js_module_name {
                    Some(wbg_js_module_name) => {
                        tracing::debug!(
                            "importing wasm-bindgen generated bindings {wbg_js_module_name}"
                        );
                        let wbg_js_promise: Promise =
                            js_sys::eval(&format!("import(\"{wbg_js_module_name}\")"))
                                .map_err(utils::Error::js)?
                                .into();
                        Some(
                            JsFuture::from(wbg_js_promise)
                                .await
                                .map_err(utils::Error::js)?,
                        )
                    }
                    None => None,
                };

                // Execute spawned thread.
                spawn_wasm
                    .execute(self.module.clone(), self.memory.clone(), wbg_mod)
                    .await?;
            }
        }

        Ok(())
    }

    /// Create busy guard.
    fn busy(&self) -> impl Drop {
        struct BusyGuard {
            id: u32,
            scheduler: Scheduler,
        }
        impl Drop for BusyGuard {
            fn drop(&mut self) {
                let _ = self.scheduler.send(SchedulerMsg::WorkerDone(self.id));
            }
        }

        BusyGuard {
            id: self.id,
            scheduler: self.scheduler.clone(),
        }
    }
}
