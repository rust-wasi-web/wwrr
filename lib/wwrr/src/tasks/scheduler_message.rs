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
    /// Message from worker that it is done.
    WorkerDone(u32),
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
    /// [`wasmer::Module`] and friends are `!Send` in practice.
    pub _not_send: PhantomData<*const ()>,
}

impl SchedulerInit {
    pub(crate) unsafe fn try_from_js(value: JsValue) -> Result<Self, Error> {
        let de = Deserializer::new(value);
        if de.ty()? != consts::TYPE_INIT {
            return Err(anyhow!("invalid init message type").into());
        }

        let msg_tx = de.boxed(consts::MSG_TX)?;
        let msg_rx = de.boxed(consts::MSG_RX)?;
        let module: WebAssembly::Module = de.js(consts::MODULE)?;
        let module_bytes: Bytes = de.boxed(consts::MODULE_BYTES)?;
        let memory: JsValue = de.js(consts::MEMORY)?;
        let memory_type: MemoryType = de.boxed(consts::MEMORY_TYPE)?;

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
            _not_send: PhantomData,
        })
    }

    pub(crate) fn into_js(self) -> Result<JsValue, Error> {
        let Self {
            msg_tx,
            msg_rx,
            module,
            memory,
            _not_send,
        } = self;

        Serializer::new(consts::TYPE_INIT)
            .boxed(consts::MSG_TX, msg_tx)
            .boxed(consts::MSG_RX, msg_rx)
            .boxed(consts::MODULE_BYTES, module.serialize())
            .set(consts::MODULE, module)
            .boxed(consts::MEMORY_TYPE, memory.ty(&wasmer::Store::default()))
            .set(consts::MEMORY, memory.as_jsvalue(&wasmer::Store::default()))
            .finish()
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
}
