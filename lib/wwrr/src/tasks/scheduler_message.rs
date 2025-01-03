use std::marker::PhantomData;

use bytes::Bytes;
use derivative::Derivative;
use js_sys::WebAssembly;
use utils::Error;
use wasm_bindgen::JsValue;
use wasmer::AsJs;

use crate::tasks::{
    interop::{Deserializer, Serializer},
    task_wasm::SpawnWasm,
    AsyncTask, LocalAsyncModuleTask, LocalAsyncTask,
};

/// Messages sent from the [`crate::tasks::ThreadPool`] handle to the
/// `Scheduler`.
#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) enum SchedulerMessage {
    /// Run a promise on a worker thread.
    SpawnAsync(#[derivative(Debug(format_with = "utils::hidden"))] AsyncTask),
    /// Run a blocking operation on a worker thread.
    SpawnBlocking(#[derivative(Debug(format_with = "utils::hidden"))] LocalAsyncTask),
    /// A message sent from a worker thread.
    /// Mark a worker as idle.
    WorkerIdle { worker_id: u32 },
    /// Mark a worker as busy.
    WorkerBusy { worker_id: u32 },
    /// Run a task in the background, explicitly transferring the
    /// [`js_sys::WebAssembly::Module`] to the worker.
    SpawnWithModule {
        module: wasmer::Module,
        #[derivative(Debug(format_with = "utils::hidden"))]
        task: LocalAsyncModuleTask,
    },
    /// Run a task in the background, explicitly transferring the
    /// [`js_sys::WebAssembly::Module`] to the worker.
    SpawnWithModuleAndMemory {
        module: wasmer::Module,
        memory: Option<wasmer::Memory>,
        spawn_wasm: SpawnWasm,
    },
    #[doc(hidden)]
    #[allow(dead_code)]
    Markers {
        /// [`wasmer::Module`] and friends are `!Send` in practice.
        not_send: PhantomData<*const ()>,
        /// Mark this variant as unreachable.
        uninhabited: std::convert::Infallible,
    },
}

impl SchedulerMessage {
    pub(crate) unsafe fn try_from_js(value: JsValue) -> Result<Self, Error> {
        let de = Deserializer::new(value);

        match de.ty()?.as_str() {
            consts::TYPE_SPAWN_ASYNC => {
                let task = de.boxed(consts::PTR)?;
                Ok(SchedulerMessage::SpawnAsync(task))
            }
            consts::TYPE_SPAWN_BLOCKING => {
                let task = de.boxed(consts::PTR)?;
                Ok(SchedulerMessage::SpawnBlocking(task))
            }
            consts::TYPE_WORKER_IDLE => {
                let worker_id = de.serde(consts::WORKER_ID)?;
                Ok(SchedulerMessage::WorkerIdle { worker_id })
            }
            consts::TYPE_WORKER_BUSY => {
                let worker_id = de.serde(consts::WORKER_ID)?;
                Ok(SchedulerMessage::WorkerBusy { worker_id })
            }
            consts::TYPE_SPAWN_WITH_MODULE => {
                let module: WebAssembly::Module = de.js(consts::MODULE)?;
                let module_bytes: Option<Bytes> = de.boxed(consts::MODULE_BYTES)?;
                let task = de.boxed(consts::PTR)?;
                Ok(SchedulerMessage::SpawnWithModule {
                    module: wasmer::Module::from_module_and_binary(module, &module_bytes.unwrap()),
                    task,
                })
            }
            consts::TYPE_SPAWN_WITH_MODULE_AND_MEMORY => {
                let spawn_wasm: SpawnWasm = de.boxed(consts::PTR)?;
                let module: WebAssembly::Module = de.js(consts::MODULE)?;
                let module_bytes: Option<Bytes> = de.boxed(consts::MODULE_BYTES)?;

                let memory = match spawn_wasm.shared_memory_type() {
                    Some(ty) => {
                        let memory: JsValue = de.js(consts::MEMORY)?;
                        let mut store = wasmer::Store::default();
                        wasmer::Memory::from_jsvalue(&mut store, &ty, &memory).ok()
                    }
                    None => None,
                };

                Ok(SchedulerMessage::SpawnWithModuleAndMemory {
                    module: wasmer::Module::from_module_and_binary(module, &module_bytes.unwrap()),
                    memory,
                    spawn_wasm,
                })
            }
            other => {
                tracing::warn!(r#type = other, "Unknown message type");
                Err(anyhow::anyhow!("Unknown message type, \"{other}\"").into())
            }
        }
    }

    pub(crate) fn into_js(self) -> Result<JsValue, Error> {
        match self {
            SchedulerMessage::SpawnAsync(task) => Serializer::new(consts::TYPE_SPAWN_ASYNC)
                .boxed(consts::PTR, task)
                .finish(),
            SchedulerMessage::SpawnBlocking(task) => Serializer::new(consts::TYPE_SPAWN_BLOCKING)
                .boxed(consts::PTR, task)
                .finish(),
            SchedulerMessage::WorkerIdle { worker_id } => Serializer::new(consts::TYPE_WORKER_IDLE)
                .set(consts::WORKER_ID, worker_id)
                .finish(),
            SchedulerMessage::WorkerBusy { worker_id } => Serializer::new(consts::TYPE_WORKER_BUSY)
                .set(consts::WORKER_ID, worker_id)
                .finish(),
            SchedulerMessage::SpawnWithModule { module, task } => {
                Serializer::new(consts::TYPE_SPAWN_WITH_MODULE)
                    .boxed(consts::MODULE_BYTES, module.serialize())
                    .set(consts::MODULE, JsValue::from(module))
                    .boxed(consts::PTR, task)
                    .finish()
            }
            SchedulerMessage::SpawnWithModuleAndMemory {
                module,
                memory,
                spawn_wasm,
            } => {
                let mut ser = Serializer::new(consts::TYPE_SPAWN_WITH_MODULE_AND_MEMORY)
                    .set(consts::MODULE, module)
                    .boxed(consts::PTR, spawn_wasm);

                if let Some(memory) = memory {
                    let store = wasmer::Store::default();
                    ser = ser.set(consts::MEMORY, memory.as_jsvalue(&store));
                }

                ser.finish()
            }
            SchedulerMessage::Markers { uninhabited, .. } => match uninhabited {},
        }
    }
}

mod consts {
    pub const TYPE_SPAWN_ASYNC: &str = "spawn-async";
    pub const TYPE_SPAWN_BLOCKING: &str = "spawn-blocking";
    pub const TYPE_WORKER_IDLE: &str = "worker-idle";
    pub const TYPE_WORKER_BUSY: &str = "worker-busy";
    pub const TYPE_SPAWN_WITH_MODULE: &str = "spawn-with-module";
    pub const TYPE_SPAWN_WITH_MODULE_AND_MEMORY: &str = "spawn-with-module-and-memory";
    pub const MEMORY: &str = "memory";
    pub const MODULE: &str = "module";
    pub const MODULE_BYTES: &str = "module-bytes";
    pub const PTR: &str = "ptr";
    pub const WORKER_ID: &str = "worker-id";
}
