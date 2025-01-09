use std::marker::PhantomData;

use anyhow::anyhow;
use bytes::Bytes;
use js_sys::WebAssembly;
use tokio::sync::{mpsc, oneshot};
use utils::Error;
use wasm_bindgen::JsValue;
use wasmer::{AsJs, MemoryType};

use crate::tasks::{
    interop::{Deserializer, Serializer},
    task_wasm::SpawnWasm,
};

/// Messages sent from the [`crate::tasks::ThreadPool`] handle to the
/// `Scheduler`.
#[derive(Debug)]
pub(crate) enum SchedulerMsg {
    /// Message from worker that it is exiting.
    WorkerExit(u32),
    /// Send a notification at the specified time.
    Sleep {
        /// Duration to sleep in milliseconds.
        duration: i32,
        /// Notify channel.
        notify: oneshot::Sender<()>,
    },
    /// Pings the scheduler.
    Ping(oneshot::Sender<()>),
    /// Spawn a thread on a new web worker.
    SpawnWasm(SpawnWasm),
}

/// Scheduler initialization message sent as web worker message.
#[derive(Debug)]
pub(crate) struct SchedulerInit {
    /// Message sender.
    pub msg_tx: mpsc::UnboundedSender<SchedulerMsg>,
    /// Message receiver.
    pub msg_rx: mpsc::UnboundedReceiver<SchedulerMsg>,
    /// WebAssembly module for spawning threads.
    pub module: wasmer::Module,
    /// WebAssembly memory for spawning threads.
    pub memory: wasmer::Memory,
    /// wasm-bindgen generated module name.
    pub wbg_js_module_name: String,
    /// [`wasmer::Module`] and friends are `!Send` in practice.
    pub _not_send: PhantomData<*const ()>,
}

impl SchedulerInit {
    pub(crate) fn into_js(self) -> Result<JsValue, Error> {
        let Self {
            msg_tx,
            msg_rx,
            module,
            memory,
            wbg_js_module_name,
            _not_send,
        } = self;

        Serializer::new(consts::TYPE_INIT)
            .boxed(consts::MSG_TX, msg_tx)
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
            return Err(anyhow!("invalid schduler init message type").into());
        }

        let msg_tx = de.boxed(consts::MSG_TX)?;
        let msg_rx = de.boxed(consts::MSG_RX)?;
        let module: WebAssembly::Module = de.js(consts::MODULE)?;
        let module_bytes: Bytes = de.boxed(consts::MODULE_BYTES)?;
        let memory: JsValue = de.js(consts::MEMORY)?;
        let memory_type: MemoryType = de.boxed(consts::MEMORY_TYPE)?;
        let wbg_js_module_name: String = de.boxed(consts::WBG_JS_MODULE_NAME)?;

        Ok(Self {
            msg_tx,
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

mod consts {
    pub const TYPE_INIT: &str = "init-scheduler";
    pub const MSG_TX: &str = "msg-tx";
    pub const MSG_RX: &str = "msg-rx";
    pub const MODULE: &str = "module";
    pub const MODULE_BYTES: &str = "module-bytes";
    pub const MEMORY: &str = "memory";
    pub const MEMORY_TYPE: &str = "memory-type";
    pub const WBG_JS_MODULE_NAME: &str = "wbg-js-module-name";
}
