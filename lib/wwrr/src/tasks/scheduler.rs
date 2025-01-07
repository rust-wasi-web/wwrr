use std::collections::HashMap;
use std::{
    fmt::Debug,
    sync::atomic::{AtomicU32, Ordering},
};

use anyhow::{anyhow, Context, Error};
use tokio::sync::{mpsc, oneshot};
use utils::GlobalScope;
use wasm_bindgen::{prelude::*, JsCast};
use wasm_bindgen_futures::JsFuture;
use wasmer::{AsJs, Memory, Module};

use super::scheduler_message::SchedulerMsg;
use crate::tasks::scheduler_message::SchedulerInit;
use crate::tasks::worker::{init_message_scheduler, WORKER_URL};
use crate::tasks::{PostMessagePayload, WorkerHandle};

/// A handle for interacting with the threadpool's scheduler.
#[derive(Debug, Clone)]
pub(crate) struct Scheduler {
    msg_tx: mpsc::UnboundedSender<SchedulerMsg>,
}

impl Scheduler {
    /// Spawn a web worker running the scheduler.
    pub fn spawn(module: Module, memory: Memory) -> Self {
        tracing::info!("Spawning task scheduler");

        // Start web worker.
        let wo = web_sys::WorkerOptions::new();
        wo.set_name("scheduler");
        wo.set_type(web_sys::WorkerType::Module);
        let worker = web_sys::Worker::new_with_options(&WORKER_URL, &wo).unwrap();

        // Send init message to load WebAssembly module.
        let wasm_init_msg = init_message_scheduler();
        worker.post_message(&wasm_init_msg).unwrap();

        // Send scheduler init message.
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let scheduler_init = SchedulerInit {
            msg_tx: msg_tx.clone(),
            msg_rx,
            module,
            memory,
            _not_send: std::marker::PhantomData,
        };
        worker
            .post_message(&scheduler_init.into_js().unwrap())
            .unwrap();

        Self { msg_tx }
    }

    /// Sends a message to the scheduler.
    pub fn send(&self, msg: SchedulerMsg) -> Result<(), Error> {
        self.msg_tx
            .send(msg)
            .map_err(|_| anyhow!("scheduler died"))?;
        Ok(())
    }

    /// Pings the scheduler and waits for a reply.
    pub async fn ping(&self) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        self.send(SchedulerMsg::Ping(tx))?;
        rx.await.context("scheduler does not respond to ping")?;
        Ok(())
    }
}

/// Scheduler worker.
#[wasm_bindgen(skip_typescript)]
struct SchedulerWorker {}

#[wasm_bindgen]
impl SchedulerWorker {
    /// Starts the scheduler on this web worker.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {}
    }

    /// Hnadles a scheduler message.
    #[wasm_bindgen]
    pub fn handle(&mut self, msg: JsValue) -> Result<(), utils::Error> {
        tracing::info!("Initializing scheduler");
        let scheduler_init = unsafe { SchedulerInit::try_from_js(msg) }?;
        SchedulerState::spawn(scheduler_init);
        Ok(())
    }
}

/// The state for the actor in charge of the threadpool.
#[derive(Debug)]
pub(crate) struct SchedulerState {
    /// Global scope.
    global: GlobalScope,
    /// Workers that are busy and cannot be reused afterwards.
    workers: HashMap<u32, WorkerHandle>,
    /// Message sender.
    msg_tx: mpsc::UnboundedSender<SchedulerMsg>,
    /// Message receiver.
    msg_rx: mpsc::UnboundedReceiver<SchedulerMsg>,
    /// WebAssembly module for spawning threads.
    module: wasmer::Module,
    /// WebAssembly memory for spawning threads.
    memory: wasmer::Memory,
}

impl SchedulerState {
    /// Runs the scheduler on this web worker.
    pub fn spawn(init: SchedulerInit) {
        let SchedulerInit {
            msg_tx,
            msg_rx,
            module,
            memory,
            _not_send,
        } = init;

        let this = Self {
            global: GlobalScope::current(),
            workers: HashMap::new(),
            msg_tx,
            msg_rx,
            module,
            memory,
        };
        wasm_bindgen_futures::spawn_local(this.run());
    }

    /// Scheduler main loop.
    async fn run(mut self) {
        while let Some(msg) = self.msg_rx.recv().await {
            if let Err(e) = self.execute(msg) {
                tracing::error!(error = &*e, "An error occurred while handling a message");
            }
        }
    }

    /// Executes a scheduler message.
    pub fn execute(&mut self, message: SchedulerMsg) -> Result<(), Error> {
        match message {
            SchedulerMsg::SpawnWasm(spawn_wasm) => {
                self.post_message(PostMessagePayload::SpawnWithModuleAndMemory {
                    module: self.module.clone(),
                    memory: Some(
                        self.memory
                            .as_jsvalue(&wasmer::Store::default())
                            .dyn_into()
                            .unwrap(),
                    ),
                    spawn_wasm,
                })
            }
            SchedulerMsg::WorkerDone(worker_id) => {
                self.workers.remove(&worker_id).unwrap();
                tracing::trace!(worker.id = worker_id, "Worker is done",);
                Ok(())
            }
            SchedulerMsg::Sleep { duration, notify } => {
                let global = self.global.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = JsFuture::from(global.sleep(duration)).await;
                    let _ = notify.send(());
                });
                Ok(())
            }
            SchedulerMsg::Ping(tx) => {
                let _ = tx.send(());
                Ok(())
            }
        }
    }

    /// Spawns a worker and sends a message to it.
    fn post_message(&mut self, msg: PostMessagePayload) -> Result<(), Error> {
        let worker = self.start_worker()?;
        worker
            .send(msg)
            .with_context(|| format!("Unable to send a message to worker {}", worker.id()))?;

        self.workers.insert(worker.id(), worker);

        Ok(())
    }

    fn start_worker(&mut self) -> Result<WorkerHandle, Error> {
        // Note: By using a monotonically incrementing counter, we can make sure
        // every single worker created with this shared linear memory will get a
        // unique ID.
        static NEXT_ID: AtomicU32 = AtomicU32::new(1);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

        let scheduler = Scheduler {
            msg_tx: self.msg_tx.clone(),
        };
        let handle = WorkerHandle::spawn(id, scheduler)?;
        Ok(handle)
    }
}
