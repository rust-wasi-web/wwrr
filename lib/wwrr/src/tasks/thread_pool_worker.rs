use std::cell::OnceCell;

use anyhow::anyhow;
use js_sys::Promise;
use wasm_bindgen::{prelude::wasm_bindgen, JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::DedicatedWorkerGlobalScope;

use crate::tasks::PostMessagePayload;

use super::scheduler::Scheduler;
use super::scheduler_message::SchedulerMsg;

/// The Rust state for a worker in the threadpool.
#[wasm_bindgen(skip_typescript)]
#[derive(Debug)]
pub struct ThreadPoolWorker {
    id: u32,
    scheduler: OnceCell<Scheduler>,
}

impl ThreadPoolWorker {
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
            scheduler: self.scheduler.get().expect("worker uninitialized").clone(),
        }
    }

    #[tracing::instrument(level = "debug", skip_all, fields(worker.id = self.id))]
    pub async fn handle(&self, msg: JsValue) -> Result<(), utils::Error> {
        // Safety: The message was created using PostMessagePayload::to_js()
        let msg = unsafe { PostMessagePayload::try_from_js(msg)? };

        tracing::trace!(?msg, "Handling a message");

        match msg {
            PostMessagePayload::Init { scheduler } => {
                tracing::info!("worker initialized");
                self.scheduler
                    .set(scheduler)
                    .map_err(|_| anyhow!("worker already initialized"))?;
            }
            PostMessagePayload::SpawnWithModuleAndMemory {
                module,
                memory,
                spawn_wasm,
            } => {
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
                let task = spawn_wasm.begin().await;
                task.execute(module, memory.into(), wbg_mod).await?;

                // Terminate web worker.
                self.close();
            }
        }

        Ok(())
    }

    /// Terminate this web worker.
    fn close(&self) {
        tracing::info!("Terminating web worker");
        let scope: DedicatedWorkerGlobalScope = js_sys::global().dyn_into().unwrap();
        scope.close();
    }
}

#[wasm_bindgen]
impl ThreadPoolWorker {
    #[wasm_bindgen(constructor)]
    pub fn new(id: u32) -> ThreadPoolWorker {
        // We need a way to give it the channel!
        ThreadPoolWorker {
            id,
            scheduler: OnceCell::new(),
        }
    }

    #[wasm_bindgen(js_name = "handle")]
    pub async fn js_handle(&self, msg: JsValue) -> Result<(), utils::Error> {
        self.handle(msg).await
    }
}
