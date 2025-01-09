use js_sys::Promise;
use tokio::sync::mpsc;
use utils::GlobalScope;
use wasm_bindgen::{prelude::wasm_bindgen, JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::DedicatedWorkerGlobalScope;

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
    /// Message receiver.
    msg_rx: mpsc::UnboundedReceiver<WorkerMsg>,
    /// WebAssembly module for spawning threads.
    module: wasmer::Module,
    /// WebAssembly memory for spawning threads.
    memory: wasmer::Memory,
    /// wasm-bindgen generated module name.
    wbg_js_module: JsValue,
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
            wbg_js_module_name,
            _not_send,
        } = init;

        wasm_bindgen_futures::spawn_local(async move {
            // Import wasm-bindgen generated JavaScript module.
            tracing::debug!(
                "worker {id} importing wasm-bindgen generated bindings {wbg_js_module_name}"
            );
            let wbg_js_promise: Promise =
                js_sys::eval(&format!("import(\"{wbg_js_module_name}\")"))
                    .expect("failed to import wasm-bindgen generated module")
                    .into();
            let wbg_js_module = JsFuture::from(wbg_js_promise)
                .await
                .expect("failed to import wasm-bindgen generated module")
                .into();

            // Send ready notification.
            ready_tx.send(()).unwrap();

            // Start worker loop.
            Self {
                id,
                msg_rx,
                module,
                memory,
                wbg_js_module,
            }
            .run()
            .await;

            // Notify scheduler that worker is exiting.
            tracing::info!("worker {id} exiting");
            let _ = scheduler.send(SchedulerMsg::WorkerExit(id));
            wasm_bindgen_futures::spawn_local(async move {
                let scope: DedicatedWorkerGlobalScope = js_sys::global().dyn_into().unwrap();
                scope.close();
            });
        });
    }

    /// Worker main loop.
    async fn run(mut self) {
        if let Some(msg) = self.msg_rx.recv().await {
            if let Err(e) = self.execute(msg).await {
                tracing::error!("An error occurred while handling a message: {e}");
            }
        }
    }

    #[tracing::instrument(level = "debug", skip_all, fields(worker.id = self.id))]
    async fn execute(&self, msg: WorkerMsg) -> Result<(), utils::Error> {
        tracing::trace!(?msg, "Handling a message");

        match msg {
            WorkerMsg::SpawnWasm(spawn_wasm) => {
                tracing::info!(
                    "spawn_wasm in worker at {} ms",
                    GlobalScope::current().now()
                );

                // Execute spawned thread.
                spawn_wasm
                    .execute(
                        self.module.clone(),
                        self.memory.clone(),
                        Some(self.wbg_js_module.clone()),
                    )
                    .await?;
            }
        }

        Ok(())
    }
}
