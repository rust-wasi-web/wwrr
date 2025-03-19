use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;
use std::pin::pin;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Error};
use tokio::sync::{mpsc, oneshot, Notify};
use wasm_bindgen::prelude::*;
use wasmer_wasix::runtime::task_manager::SchedulerSpawn;
use web_sys::DedicatedWorkerGlobalScope;

use super::scheduler_message::SchedulerMsg;
use super::worker_message::WorkerMsg;
use crate::register_panic_hook;
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
    pub fn spawn(scheduler_spawn: SchedulerSpawn) -> Self {
        let SchedulerSpawn {
            module,
            memory,
            wbg_js_module_name,
            prestarted_workers,
        } = scheduler_spawn;

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
            prestarted_workers,
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
    /// Preinitializes the scheduler.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        register_panic_hook();
        Self {}
    }

    /// Handles the init message and starts the scheduler.
    #[wasm_bindgen]
    pub fn handle(&mut self, msg: JsValue) -> Result<(), utils::Error> {
        let scheduler_init = unsafe { SchedulerInit::try_from_js(msg) }?;
        SchedulerState::spawn(scheduler_init);
        Ok(())
    }
}

/// The state for the actor in charge of the threadpool.
#[derive(Debug)]
pub(crate) struct SchedulerState {
    /// Next worker id.
    next_worker_id: u32,
    /// Workers that are busy and cannot be reused afterwards.
    active_workers: HashMap<u32, WorkerHandle>,
    /// Workers that are ready to be used.
    ready_workers: Arc<Mutex<VecDeque<WorkerHandle>>>,
    /// Notification that a worker has been added to `ready_workers`.
    worker_ready: Arc<Notify>,
    /// Message sender.
    msg_tx: mpsc::UnboundedSender<SchedulerMsg>,
    /// Message receiver.
    msg_rx: mpsc::UnboundedReceiver<SchedulerMsg>,
    /// WebAssembly module for spawning threads.
    module: wasmer::Module,
    /// WebAssembly memory for spawning threads.
    memory: wasmer::Memory,
    /// Number of workers to pre-start.
    prestarted_workers: usize,
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
            prestarted_workers,
            _not_send,
        } = init;

        let this = Self {
            next_worker_id: 1,
            active_workers: HashMap::new(),
            ready_workers: Arc::new(Mutex::new(VecDeque::new())),
            worker_ready: Arc::new(Notify::new()),
            msg_tx,
            msg_rx,
            module,
            memory,
            wbg_js_module_name,
            prestarted_workers,
        };
        wasm_bindgen_futures::spawn_local(this.run());
    }

    /// Scheduler main loop.
    async fn run(mut self) {
        tracing::trace!("pre-starting {} workers", self.prestarted_workers);
        for _ in 0..self.prestarted_workers {
            self.start_worker();
        }
        while self.ready_workers.lock().unwrap().len() < self.prestarted_workers {
            self.worker_ready.notified().await;
        }

        while let Some(msg) = self.msg_rx.recv().await {
            if let Err(e) = self.execute(msg).await {
                tracing::error!(error = &*e, "An error occurred while handling a message");
            }
        }

        tracing::debug!("scheduler exiting");
        wasm_bindgen_futures::spawn_local(async move {
            let scope: DedicatedWorkerGlobalScope = js_sys::global().dyn_into().unwrap();
            scope.close();
        });
    }

    /// Executes a scheduler message.
    pub async fn execute(&mut self, message: SchedulerMsg) -> Result<(), Error> {
        match message {
            SchedulerMsg::SpawnWasm(spawn_wasm) => {
                self.post_message(WorkerMsg::SpawnWasm(spawn_wasm)).await?;
                Ok(())
            }
            SchedulerMsg::WorkerExit(worker_id) => {
                let mut worker = self.active_workers.remove(&worker_id).unwrap();
                worker.set_terminate(false);
                tracing::trace!(worker.id = worker_id, "Worker has exited");
                Ok(())
            }
            SchedulerMsg::Ping(tx) => {
                let _ = tx.send(());
                Ok(())
            }
        }
    }

    /// Spawns a worker and sends a message to it.
    async fn post_message(&mut self, msg: WorkerMsg) -> Result<(), Error> {
        let worker = self.take_worker().await;
        worker
            .send(msg)
            .with_context(|| format!("Unable to send a message to worker {}", worker.id()))?;

        self.active_workers.insert(worker.id(), worker);

        Ok(())
    }

    /// Starts a new worker in the background and adds it to the pool of ready workers
    /// once it is initialized.
    fn start_worker(&mut self) {
        let id = self.next_worker_id;
        let ready_workers = self.ready_workers.clone();
        let worker_ready = self.worker_ready.clone();
        let msg_tx = self.msg_tx.clone();
        let module = self.module.clone();
        let memory = self.memory.clone();
        let wbg_js_module_name = self.wbg_js_module_name.clone();

        self.next_worker_id += 1;

        wasm_bindgen_futures::spawn_local(async move {
            let scheduler = Scheduler { msg_tx };
            let handle = WorkerHandle::spawn(id, scheduler, module, memory, wbg_js_module_name)
                .await
                .expect("starting thread worker failed");

            ready_workers.lock().unwrap().push_back(handle);
            worker_ready.notify_one();
        });
    }

    /// Takes a worker from the pool of ready workers and starts a new worker
    /// to refill the pool.
    ///
    /// Waits until a ready worker becomes available.
    async fn take_worker(&mut self) -> WorkerHandle {
        self.start_worker();

        let mut waited = false;
        loop {
            let mut notified = pin!(self.worker_ready.notified());
            notified.as_mut().enable();

            let worker_opt = self.ready_workers.lock().unwrap().pop_front();
            if let Some(worker) = worker_opt {
                if waited {
                    if self.prestarted_workers > 0 {
                        tracing::info!("worker has become available");
                    } else {
                        tracing::debug!("worker has become available");
                    };
                }
                break worker;
            }

            if self.prestarted_workers > 0 {
                tracing::warn!("thread pool has run out of pre-started workers");
            } else {
                tracing::debug!("waiting for worker to become available");
            }
            notified.await;
            waited = true;
        }
    }
}
