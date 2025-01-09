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
    /// wasm-bindgen generated module name.
    pub wbg_js_module_name: String,
    /// [`wasmer::Module`] and friends are `!Send` in practice.
    pub _not_send: PhantomData<*const ()>,
}

mod consts {
    pub const TYPE_INIT: &str = "init-worker";
    pub const SCHEDULER: &str = "scheduler";
    pub const READY_TX: &str = "ready-tx";
    pub const MSG_RX: &str = "msg-rx";
    pub const MODULE: &str = "module";
    pub const MODULE_BYTES: &str = "module-bytes";
    pub const MEMORY: &str = "memory";
    pub const MEMORY_TYPE: &str = "memory-type";
    pub const WBG_JS_MODULE_NAME: &str = "wbg-js-module-name";
}

impl WorkerInit {
    pub(crate) fn into_js(self) -> Result<JsValue, Error> {
        let Self {
            scheduler,
            ready_tx,
            msg_rx,
            module,
            memory,
            wbg_js_module_name,
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
            .boxed(consts::WBG_JS_MODULE_NAME, wbg_js_module_name)
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
        let wbg_js_module_name: String = de.boxed(consts::WBG_JS_MODULE_NAME)?;

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
            wbg_js_module_name,
            _not_send: PhantomData,
        })
    }
}
