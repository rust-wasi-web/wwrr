use js_sys::Promise;
use wasm_bindgen::{prelude::wasm_bindgen, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::tasks::{AsyncJob, BlockingJob, PostMessagePayload, WorkerMessage};

/// The Rust state for a worker in the threadpool.
#[wasm_bindgen(skip_typescript)]
#[derive(Debug)]
pub struct ThreadPoolWorker {
    id: u32,
}

impl ThreadPoolWorker {
    fn busy(&self) -> impl Drop {
        struct BusyGuard;
        impl Drop for BusyGuard {
            fn drop(&mut self) {
                let _ = WorkerMessage::MarkIdle.emit();
            }
        }

        let _ = WorkerMessage::MarkBusy.emit();

        BusyGuard
    }

    #[tracing::instrument(level = "debug", skip_all, fields(worker.id = self.id))]
    pub async fn handle(&self, msg: JsValue) -> Result<(), crate::utils::Error> {
        // Safety: The message was created using PostMessagePayload::to_js()
        let msg = unsafe { PostMessagePayload::try_from_js(msg)? };

        tracing::trace!(?msg, "Handling a message");

        match msg {
            PostMessagePayload::Async(async_job) => self.execute_async(async_job).await,
            PostMessagePayload::Blocking(blocking) => self.execute_blocking(blocking).await,
        }
    }

    async fn execute_async(&self, job: AsyncJob) -> Result<(), crate::utils::Error> {
        match job {
            AsyncJob::Thunk(thunk) => {
                thunk().await;
            }
        }

        Ok(())
    }

    // TODO: change to async
    async fn execute_blocking(&self, job: BlockingJob) -> Result<(), crate::utils::Error> {
        match job {
            BlockingJob::Thunk(thunk) => {
                let _guard = self.busy();
                thunk();
            }
            BlockingJob::SpawnWithModule { module, task } => {
                let _guard = self.busy();
                task(module.into());
            }
            BlockingJob::SpawnWithModuleAndMemory {
                module,
                memory,
                spawn_wasm,
            } => {
                let wbg_mod = match &spawn_wasm.env.wbg_js_module_name {
                    Some(wbg_js_module_name) => {
                        tracing::debug!(
                            "importing wasm-bindgen generated bindings {wbg_js_module_name}"
                        );
                        let wbg_js_promise: Promise =
                            js_sys::eval(&format!("import(\"{wbg_js_module_name}\")"))
                                .map_err(crate::utils::Error::js)?
                                .into();
                        Some(
                            JsFuture::from(wbg_js_promise)
                                .await
                                .map_err(crate::utils::Error::js)?,
                        )
                    }
                    None => None,
                };

                let task = spawn_wasm.begin().await;
                let _guard = self.busy();
                task.execute(module, memory.into(), wbg_mod).await?;
            }
        }

        Ok(())
    }
}

#[wasm_bindgen]
impl ThreadPoolWorker {
    #[wasm_bindgen(constructor)]
    pub fn new(id: u32) -> ThreadPoolWorker {
        ThreadPoolWorker { id }
    }

    #[wasm_bindgen(js_name = "handle")]
    pub async fn js_handle(&self, msg: JsValue) -> Result<(), crate::utils::Error> {
        self.handle(msg).await
    }
}
