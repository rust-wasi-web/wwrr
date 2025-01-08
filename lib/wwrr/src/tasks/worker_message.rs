use std::marker::PhantomData;

use anyhow::anyhow;
use bytes::Bytes;
use js_sys::WebAssembly;
use tokio::sync::{mpsc, oneshot};
use utils::Error;
use wasm_bindgen::JsValue;
use wasmer::{AsJs, MemoryType};

use super::interop::Deserializer;
use super::scheduler::Scheduler;
use crate::tasks::{interop::Serializer, task_wasm::SpawnWasm};

/// A message sent from scheduler to worker.
#[derive(Debug)]
pub(crate) enum WorkerMsg {
    /// Spawn a thread.
    SpawnWasm(SpawnWasm),
}

/// A message that will be sent from the scheduler to init a worker.
#[derive(Debug)]
pub(crate) struct WorkerInit {
    /// Scheduler
    pub scheduler: Scheduler,
    /// Ready notification to scheduler.
    pub ready_tx: oneshot::Sender<()>,
    /// Message receiver.
    pub msg_rx: mpsc::UnboundedReceiver<WorkerMsg>,
    /// WebAssembly module for thread.
    pub module: wasmer::Module,
    /// WebAssembly memory for thread.
    pub memory: wasmer::Memory,
    /// [`wasmer::Module`] and friends are `!Send` in practice.
    pub _not_send: PhantomData<*const ()>,
}

mod consts {
    pub(crate) const TYPE_INIT: &str = "init-worker";
    pub(crate) const SCHEDULER: &str = "scheduler";
    pub(crate) const READY_TX: &str = "ready-tx";
    pub(crate) const MSG_RX: &str = "msg-rx";
    pub(crate) const MODULE: &str = "module";
    pub(crate) const MODULE_BYTES: &str = "module-bytes";
    pub(crate) const MEMORY: &str = "memory";
    pub(crate) const MEMORY_TYPE: &str = "memory-type";
}

impl WorkerInit {
    pub(crate) fn into_js(self) -> Result<JsValue, Error> {
        let Self {
            scheduler,
            ready_tx,
            msg_rx,
            module,
            memory,
            _not_send,
        } = self;
        Serializer::new(consts::TYPE_INIT)
            .boxed(consts::SCHEDULER, scheduler)
            .boxed(consts::READY_TX, ready_tx)
            .boxed(consts::MSG_RX, msg_rx)
            .boxed(consts::MODULE_BYTES, module.serialize())
            .set(consts::MODULE, module)
            .boxed(consts::MEMORY_TYPE, memory.ty(&wasmer::Store::default()))
            .set(consts::MEMORY, memory.as_jsvalue(&wasmer::Store::default()))
            .finish()
    }

    pub(crate) unsafe fn try_from_js(value: JsValue) -> Result<Self, Error> {
        let de = Deserializer::new(value);
        if de.ty()? != consts::TYPE_INIT {
            return Err(anyhow!("invalid worker init message type").into());
        }

        let scheduler: Scheduler = de.boxed(consts::SCHEDULER)?;
        let ready_tx: oneshot::Sender<()> = de.boxed(consts::READY_TX)?;
        let msg_rx: mpsc::UnboundedReceiver<WorkerMsg> = de.boxed(consts::MSG_RX)?;
        let module: WebAssembly::Module = de.js(consts::MODULE)?;
        let module_bytes: Bytes = de.boxed(consts::MODULE_BYTES)?;
        let memory: JsValue = de.js(consts::MEMORY)?;
        let memory_type: MemoryType = de.boxed(consts::MEMORY_TYPE)?;

        Ok(Self {
            scheduler,
            ready_tx,
            msg_rx,
            module: wasmer::Module::from_module_and_binary(module, &module_bytes),
            memory: wasmer::Memory::from_jsvalue(
                &mut wasmer::Store::default(),
                &memory_type,
                &memory,
            )
            .map_err(Error::js)?,
            _not_send: PhantomData,
        })
    }
}
