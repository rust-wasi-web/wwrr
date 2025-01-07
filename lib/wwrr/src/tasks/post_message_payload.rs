use bytes::Bytes;
use js_sys::WebAssembly;
use wasm_bindgen::JsValue;

use super::scheduler::Scheduler;
use crate::tasks::{interop::Serializer, task_wasm::SpawnWasm};

/// A message that will be sent from the scheduler to a worker using
/// `postMessage()`.
#[derive(Debug)]
pub(crate) enum PostMessagePayload {
    /// Initialize worker.
    Init { scheduler: Scheduler },
    /// Spawn a thread.
    SpawnWithModuleAndMemory {
        module: wasmer::Module,
        /// An instance of the WebAssembly linear memory that has already been
        /// created.
        memory: Option<WebAssembly::Memory>,
        spawn_wasm: SpawnWasm,
    },
}

mod consts {
    pub(crate) const TYPE_INIT: &str = "init-worker";
    pub(crate) const TYPE_SPAWN_WITH_MODULE_AND_MEMORY: &str = "spawn-with-module-and-memory";
    pub(crate) const SCHEDULER: &str = "scheduler";
    pub(crate) const PTR: &str = "ptr";
    pub(crate) const MODULE: &str = "module";
    pub(crate) const MODULE_BYTES: &str = "module-bytes";
    pub(crate) const MEMORY: &str = "memory";
}

impl PostMessagePayload {
    pub(crate) fn into_js(self) -> Result<JsValue, utils::Error> {
        match self {
            PostMessagePayload::Init { scheduler } => Serializer::new(consts::TYPE_INIT)
                .boxed(consts::SCHEDULER, scheduler)
                .finish(),
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
            consts::TYPE_INIT => {
                let scheduler: Scheduler = de.boxed(consts::SCHEDULER)?;
                Ok(PostMessagePayload::Init { scheduler })
            }
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
