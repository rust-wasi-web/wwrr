use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::{
    fmt::Debug,
    sync::atomic::{AtomicU32, Ordering},
};

use anyhow::{anyhow, Context, Error};
use tokio::sync::{mpsc, oneshot};
use utils::GlobalScope;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use wasmer::{Memory, Module};

use super::scheduler_message::SchedulerMsg;
use super::worker_message::WorkerMsg;
use crate::tasks::scheduler_message::SchedulerInit;
use crate::tasks::worker::{init_message_scheduler, WORKER_URL};
use crate::tasks::WorkerHandle;

/// A handle for interacting with the threadpool's scheduler.
#[derive(Debug, Clone)]
pub(crate) struct Scheduler {
    msg_tx: mpsc::UnboundedSender<SchedulerMsg>,
}

impl Scheduler {
    /// Spawn a web worker running the scheduler.
    pub fn spawn(module: Module, memory: Memory, wbg_js_module_name: String) -> Self {
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
            wbg_js_module_name,
            _not_send: std::marker::PhantomData,
        };
        worker
            .post_message(&scheduler_init.into_js().unwrap())
            .unwrap();

        Self { msg_tx }
    }

    /// Sends a message to the scheduler.
    pub fn send(&self, msg: SchedulerMsg) -> Result<(), Error> {
        tracing::info!(
            "send scheduler message {msg:?} at {} ms",
            GlobalScope::current().now()
        );
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
    /// Preinitializes the scheduler.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {}
    }

    /// Handles the init message and starts the scheduler.
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
    active_workers: HashMap<u32, WorkerHandle>,
    /// Workers that are ready to be used.
    ready_workers: Arc<Mutex<VecDeque<WorkerHandle>>>,
    /// Message sender.
    msg_tx: mpsc::UnboundedSender<SchedulerMsg>,
    /// Message receiver.
    msg_rx: mpsc::UnboundedReceiver<SchedulerMsg>,
    /// WebAssembly module for spawning threads.
    module: wasmer::Module,
    /// WebAssembly memory for spawning threads.
    memory: wasmer::Memory,
    /// wasm-bindgen generated module name.
    wbg_js_module_name: String,
}

impl SchedulerState {
    /// Runs the scheduler on this web worker.
    pub fn spawn(init: SchedulerInit) {
        let SchedulerInit {
            msg_tx,
            msg_rx,
            module,
            memory,
            wbg_js_module_name,
            _not_send,
        } = init;

        let this = Self {
            global: GlobalScope::current(),
            active_workers: HashMap::new(),
            ready_workers: Arc::new(Mutex::new(VecDeque::new())),
            msg_tx,
            msg_rx,
            module,
            memory,
            wbg_js_module_name,
        };
        wasm_bindgen_futures::spawn_local(this.run());
    }

    /// Scheduler main loop.
    async fn run(mut self) {
        tracing::info!("prespawning workers");
        for _ in 1..=8 {
            let worker = Self::start_worker(
                self.msg_tx.clone(),
                self.module.clone(),
                self.memory.clone(),
                self.wbg_js_module_name.clone(),
            )
            .await
            .unwrap();
            self.ready_workers.lock().unwrap().push_back(worker);
        }

        while let Some(msg) = self.msg_rx.recv().await {
            if let Err(e) = self.execute(msg) {
                tracing::error!(error = &*e, "An error occurred while handling a message");
            }
        }

        tracing::info!("scheduler exiting");
    }

    /// Executes a scheduler message.
    pub fn execute(&mut self, message: SchedulerMsg) -> Result<(), Error> {
        match message {
            SchedulerMsg::SpawnWasm(spawn_wasm) => {
                tracing::info!(
                    "scheduler spawn module at {} ms",
                    GlobalScope::current().now()
                );
                self.post_message(WorkerMsg::SpawnWasm(spawn_wasm))?;
                Ok(())
            }
            SchedulerMsg::WorkerExit(worker_id) => {
                let mut worker = self.active_workers.remove(&worker_id).unwrap();
                worker.set_terminate(false);
                tracing::trace!(worker.id = worker_id, "Worker has exited");
                Ok(())
            }
            SchedulerMsg::Sleep { duration, notify } => {
                let global = self.global.clone();
                tracing::info!("sleep at scheduler for {duration} ms starting at {} ms", global.now());

                // Do we need that kind of sleep?
                // I.e. using the global event loop in Firefoxy?
                // Can't we just add a sleeper here?
                // I.e. we could select over a sleep?
                // Would a Tokio sleep work?
                // No, because we don't have any runtime.
                // So can we write our own sleep future?
                // We have the condvar wait stuff in WASM.
                // So is there a way to make this a future?
                // So using the condvar thingy will block.
                // Yeah, not easy I guess!
                // Or maybe?!
                // Question is if we really want this sleep thingy here?
                // I mean we could use WASM sleep?
                



                wasm_bindgen_futures::spawn_local(async move {
                    tracing::info!("sleep at scheduler for {duration} ms starting in spawned future at {} ms", global.now());
                    let _ = JsFuture::from(global.sleep(duration)).await;
                    tracing::info!("sleep at scheduler for {duration} ms done at {} ms", global.now());
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
    fn post_message(&mut self, msg: WorkerMsg) -> Result<(), Error> {
        let ready_workers = self.ready_workers.clone();

        let worker = ready_workers
            .lock()
            .unwrap()
            .pop_front()
            .expect("no workers available");
        worker
            .send(msg)
            .with_context(|| format!("Unable to send a message to worker {}", worker.id()))?;

        self.active_workers.insert(worker.id(), worker);

        // Refill worker pool.
        // let msg_tx = self.msg_tx.clone();
        // let module = self.module.clone();
        // let memory = self.memory.clone();
        // let wbg_js_module_name = self.wbg_js_module_name.clone();
        // wasm_bindgen_futures::spawn_local(async move {
        //     let new_worker = Self::start_worker(msg_tx, module, memory, wbg_js_module_name)
        //         .await
        //         .unwrap();
        //     ready_workers.lock().unwrap().push_back(new_worker);
        // });

        Ok(())
    }

    async fn start_worker(
        msg_tx: mpsc::UnboundedSender<SchedulerMsg>,
        module: wasmer::Module,
        memory: wasmer::Memory,
        wbg_js_module_name: String,
    ) -> Result<WorkerHandle, Error> {
        // Note: By using a monotonically incrementing counter, we can make sure
        // every single worker created with this shared linear memory will get a
        // unique ID.
        static NEXT_ID: AtomicU32 = AtomicU32::new(1);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

        let scheduler = Scheduler { msg_tx };
        let handle = WorkerHandle::spawn(id, scheduler, module, memory, wbg_js_module_name).await?;
        Ok(handle)
    }
}
