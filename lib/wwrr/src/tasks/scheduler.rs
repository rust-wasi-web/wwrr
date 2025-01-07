use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::sync::LazyLock;
use std::{
    fmt::Debug,
    sync::atomic::{AtomicU32, Ordering},
};

use anyhow::{Context, Error};
use tokio::sync::oneshot;
use utils::GlobalScope;
use wasm_bindgen::{prelude::*, JsCast};
use wasm_bindgen_futures::JsFuture;
use wasmer::AsJs;

use crate::tasks::worker::{init_message_scheduler, WORKER_URL};
use crate::tasks::{PostMessagePayload, SchedulerMessage, WorkerHandle, WorkerMessage};

/// The scheduler instance.
pub static SCHEDULER: LazyLock<Scheduler> = LazyLock::new(Scheduler::spawn);

/// A handle for interacting with the threadpool's scheduler.
#[derive(Debug, Clone)]
pub(crate) struct Scheduler {}

thread_local! {
    /// Web worker instance that hosts the scheduler.
    ///
    /// This is only set in the main thread.
    static WORKER: OnceCell<web_sys::Worker> = OnceCell::new();
}

impl Scheduler {
    /// Spawn a web worker running the scheduler.
    fn spawn() -> Scheduler {
        tracing::info!("Spawning task scheduler");

        // Start web worker.
        let wo = web_sys::WorkerOptions::new();
        wo.set_name("scheduler");
        wo.set_type(web_sys::WorkerType::Module);
        let worker = web_sys::Worker::new_with_options(&WORKER_URL, &wo).unwrap();

        // Send init message.
        let init_msg = init_message_scheduler();
        worker.post_message(&init_msg).unwrap();

        WORKER.with(|w| w.set(worker)).unwrap();
        Self {}
    }

    /// Sends a message to the scheduler.
    pub fn send(&self, msg: SchedulerMessage) -> Result<(), Error> {
        WORKER.with(|worker| {
            match worker.get() {
                Some(worker) => {
                    // We are in the main thread and must send the message to the
                    // scheduler web worker.
                    let js_msg = msg.into_js().map_err(|e| e.into_anyhow())?;
                    worker
                        .post_message(&js_msg)
                        .map_err(|e| utils::Error::js(e).into_anyhow())?;
                    Ok(())
                }
                None => {
                    // We are in a child worker so we need to emit the message via
                    // postMessage() and let the WorkerHandle forward it to the
                    // scheduler.
                    WorkerMessage::Scheduler(msg)
                        .emit()
                        .map_err(|e| e.into_anyhow())?;
                    Ok(())
                }
            }
        })
    }

    /// Pings the scheduler and waits for a reply.
    pub async fn ping(&self) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        self.send(SchedulerMessage::Ping { reply: tx })?;
        rx.await.context("scheduler is dead")?;
        Ok(())
    }
}

thread_local! {
    /// The scheduler running on this web worker.
    pub static SCHEDULER_STATE: OnceCell<RefCell<SchedulerState>> = OnceCell::new();
}

/// Scheduler worker.
#[wasm_bindgen(skip_typescript)]
struct SchedulerWorker {}

#[wasm_bindgen]
impl SchedulerWorker {
    /// Starts the scheduler on this web worker.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        SCHEDULER_STATE.with(|ss| ss.set(RefCell::new(SchedulerState::new())).unwrap());
        Self {}
    }

    /// Hnadles a scheduler message.
    #[wasm_bindgen]
    pub fn handle(&self, msg: JsValue) -> Result<(), utils::Error> {
        SCHEDULER_STATE.with(|ss| ss.get().unwrap().borrow_mut().handle(msg))
    }
}

/// The state for the actor in charge of the threadpool.
#[derive(Debug)]
pub(crate) struct SchedulerState {
    /// Global scope.
    global: GlobalScope,
    /// Workers that are busy and cannot be reused afterwards.
    workers: HashMap<u32, WorkerHandle>,
}

impl SchedulerState {
    /// Runs the scheduler on this web worker.
    pub fn new() -> Self {
        SchedulerState {
            global: GlobalScope::current(),
            workers: HashMap::new(),
        }
    }

    /// Parses a web worker message and then executes it.
    pub fn handle(&mut self, msg: JsValue) -> Result<(), utils::Error> {
        let msg = unsafe { SchedulerMessage::try_from_js(msg)? };

        tracing::debug!(?msg, "Executing a message");
        if let Err(e) = self.execute(msg) {
            tracing::error!(error = &*e, "An error occurred while handling a message");
        }

        Ok(())
    }

    /// Executes a scheduler message.
    pub fn execute(&mut self, message: SchedulerMessage) -> Result<(), Error> {
        match message {
            SchedulerMessage::SpawnWithModuleAndMemory {
                module,
                memory,
                spawn_wasm,
            } => {
                let temp_store = wasmer::Store::default();
                let memory = memory.map(|m| m.as_jsvalue(&temp_store).dyn_into().unwrap());

                self.post_message(PostMessagePayload::SpawnWithModuleAndMemory {
                    module,
                    memory,
                    spawn_wasm,
                })
            }
            SchedulerMessage::WorkerDone { worker_id } => {
                self.workers.remove(&worker_id).unwrap();
                tracing::trace!(worker.id = worker_id, "Worker is done",);
                Ok(())
            }
            SchedulerMessage::Sleep { duration, notify } => {
                let global = self.global.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = JsFuture::from(global.sleep(duration)).await;
                    let _ = notify.send(());
                });
                Ok(())
            }
            SchedulerMessage::Ping { reply } => {
                let _ = reply.send(());
                Ok(())
            }
            SchedulerMessage::Markers { uninhabited, .. } => match uninhabited {},
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

        let handle = WorkerHandle::spawn(id)?;
        Ok(handle)
    }
}
