use std::marker::PhantomData;

use bytes::Bytes;
use derivative::Derivative;
use js_sys::WebAssembly;
use tokio::sync::oneshot;
use utils::Error;
use wasm_bindgen::JsValue;
use wasmer::AsJs;

use crate::tasks::{
    interop::{Deserializer, Serializer},
    task_wasm::SpawnWasm,
};

/// Messages sent from the [`crate::tasks::ThreadPool`] handle to the
/// `Scheduler`.
#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) enum SchedulerMessage {
    /// Message from worker that it is done.
    WorkerDone { worker_id: u32 },
    /// Run a task in the background, explicitly transferring the
    /// [`js_sys::WebAssembly::Module`] to the worker.
    SpawnWithModuleAndMemory {
        module: wasmer::Module,
        memory: Option<wasmer::Memory>,
        spawn_wasm: SpawnWasm,
    },
    /// Send a notification at the specified time.
    Sleep {
        /// Duration to sleep in milliseconds.
        duration: i32,
        /// Notify channel.
        notify: oneshot::Sender<()>,
    },
    /// Pings the scheduler.
    Ping {
        /// Reply channel.
        reply: oneshot::Sender<()>,
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
            consts::TYPE_WORKER_DONE => {
                let worker_id = de.serde(consts::WORKER_ID)?;
                Ok(SchedulerMessage::WorkerDone { worker_id })
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
            consts::TYPE_SLEEP => {
                let duration = de.serde(consts::DURATION)?;
                let notify = de.boxed(consts::NOTIFY)?;
                Ok(SchedulerMessage::Sleep { duration, notify })
            }
            consts::TYPE_PING => {
                let reply = de.boxed(consts::REPLY)?;
                Ok(SchedulerMessage::Ping { reply })
            }
            other => {
                tracing::warn!(r#type = other, "Unknown message type");
                Err(anyhow::anyhow!("Unknown message type, \"{other}\"").into())
            }
        }
    }

    pub(crate) fn into_js(self) -> Result<JsValue, Error> {
        match self {
            SchedulerMessage::WorkerDone { worker_id } => Serializer::new(consts::TYPE_WORKER_DONE)
                .set(consts::WORKER_ID, worker_id)
                .finish(),
            SchedulerMessage::SpawnWithModuleAndMemory {
                module,
                memory,
                spawn_wasm,
            } => {
                let mut ser = Serializer::new(consts::TYPE_SPAWN_WITH_MODULE_AND_MEMORY)
                    .boxed(consts::MODULE_BYTES, module.serialize())
                    .set(consts::MODULE, module)
                    .boxed(consts::PTR, spawn_wasm);

                if let Some(memory) = memory {
                    let store = wasmer::Store::default();
                    ser = ser.set(consts::MEMORY, memory.as_jsvalue(&store));
                }

                ser.finish()
            }
            SchedulerMessage::Sleep { duration, notify } => Serializer::new(consts::TYPE_SLEEP)
                .set(consts::DURATION, duration)
                .boxed(consts::NOTIFY, notify)
                .finish(),
            SchedulerMessage::Ping { reply } => Serializer::new(consts::TYPE_PING)
                .boxed(consts::REPLY, reply)
                .finish(),
            SchedulerMessage::Markers { uninhabited, .. } => match uninhabited {},
        }
    }
}

mod consts {
    pub const TYPE_WORKER_DONE: &str = "worker-done";
    pub const TYPE_SPAWN_WITH_MODULE_AND_MEMORY: &str = "spawn-with-module-and-memory";
    pub const TYPE_SLEEP: &str = "sleep";
    pub const TYPE_PING: &str = "ping";
    pub const MEMORY: &str = "memory";
    pub const MODULE: &str = "module";
    pub const MODULE_BYTES: &str = "module-bytes";
    pub const PTR: &str = "ptr";
    pub const WORKER_ID: &str = "worker-id";
    pub const DURATION: &str = "duration";
    pub const NOTIFY: &str = "notify";
    pub const REPLY: &str = "reply";
}
