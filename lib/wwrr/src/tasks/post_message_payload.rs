use bytes::Bytes;
use js_sys::WebAssembly;
use wasm_bindgen::JsValue;

use crate::tasks::{interop::Serializer, task_wasm::SpawnWasm};

/// A message that will be sent from the scheduler to a worker using
/// `postMessage()`.
#[derive(Debug)]
pub(crate) enum PostMessagePayload {
    SpawnWithModuleAndMemory {
        module: wasmer::Module,
        /// An instance of the WebAssembly linear memory that has already been
        /// created.
        memory: Option<WebAssembly::Memory>,
        spawn_wasm: SpawnWasm,
    },
}

mod consts {
    pub(crate) const TYPE_SPAWN_WITH_MODULE_AND_MEMORY: &str = "spawn-with-module-and-memory";
    pub(crate) const PTR: &str = "ptr";
    pub(crate) const MODULE: &str = "module";
    pub(crate) const MODULE_BYTES: &str = "module-bytes";
    pub(crate) const MEMORY: &str = "memory";
}

impl PostMessagePayload {
    pub(crate) fn into_js(self) -> Result<JsValue, utils::Error> {
        match self {
            PostMessagePayload::SpawnWithModuleAndMemory {
                module,
                memory,
                spawn_wasm,
            } => Serializer::new(consts::TYPE_SPAWN_WITH_MODULE_AND_MEMORY)
                .boxed(consts::PTR, spawn_wasm)
                .boxed(consts::MODULE_BYTES, module.serialize())
                .set(consts::MODULE, JsValue::from(module))
                .set(consts::MEMORY, memory)
                .finish(),
        }
    }

    /// Try to convert a [`PostMessagePayload`] back from a [`JsValue`].
    ///
    /// # Safety
    ///
    /// This can only be called if the original [`JsValue`] was created using
    /// [`PostMessagePayload::into_js()`].
    pub(crate) unsafe fn try_from_js(value: JsValue) -> Result<Self, utils::Error> {
        let de = crate::tasks::interop::Deserializer::new(value);

        // Safety: Keep this in sync with PostMessagePayload::to_js()
        match de.ty()?.as_str() {
            consts::TYPE_SPAWN_WITH_MODULE_AND_MEMORY => {
                let module: WebAssembly::Module = de.js(consts::MODULE)?;
                let module_bytes: Option<Bytes> = de.boxed(consts::MODULE_BYTES)?;
                let memory = de.js(consts::MEMORY).ok();
                let spawn_wasm = de.boxed(consts::PTR)?;

                Ok(PostMessagePayload::SpawnWithModuleAndMemory {
                    module: wasmer::Module::from_module_and_binary(module, &module_bytes.unwrap()),
                    memory,
                    spawn_wasm,
                })
            }
            other => Err(anyhow::anyhow!("Unknown message type: {other}").into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use futures::FutureExt;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_test::wasm_bindgen_test;
    use wasmer::AsJs;
    use wasmer_wasix::{runtime::task_manager::TaskWasm, WasiEnvBuilder};

    use crate::{runtime::Runtime, tasks::SchedulerMessage};

    use super::*;

    static ENVVAR_WASM: &[u8] = include_bytes!("../../../../test-assets/envvar.wasm");

    #[wasm_bindgen_test]
    async fn round_trip_spawn_with_module_and_memory() {
        let module = wasmer::Module::new(ENVVAR_WASM).await.unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let runtime = Runtime::new();
        let env = WasiEnvBuilder::new("program")
            .runtime(Arc::new(runtime))
            .build()
            .unwrap();
        let msg = crate::tasks::task_wasm::to_scheduler_message(TaskWasm::new(
            Box::new({
                let flag = Arc::clone(&flag);
                move |_| {
                    async move {
                        flag.store(true, Ordering::SeqCst);
                    }
                    .boxed_local()
                }
            }),
            env,
            module,
        ))
        .unwrap();
        let msg = match msg {
            SchedulerMessage::SpawnWithModuleAndMemory {
                module,
                memory,
                spawn_wasm,
            } => PostMessagePayload::SpawnWithModuleAndMemory {
                module: module.into(),
                memory: memory.map(|m| m.as_jsvalue(&wasmer::Store::default()).dyn_into().unwrap()),
                spawn_wasm,
            },
            _ => unreachable!(),
        };

        let js = msg.into_js().unwrap();
        let round_tripped = unsafe { PostMessagePayload::try_from_js(js).unwrap() };

        let (module, memory, spawn_wasm) = match round_tripped {
            PostMessagePayload::SpawnWithModuleAndMemory {
                module,
                memory,
                spawn_wasm,
            } => (module, memory, spawn_wasm),
        };
        spawn_wasm
            .begin()
            .await
            .execute(module, memory.into(), None)
            .await
            .unwrap();
        assert!(flag.load(Ordering::SeqCst));
    }
}
