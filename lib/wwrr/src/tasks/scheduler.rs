use std::sync::{LazyLock, OnceLock};
use std::{
    collections::VecDeque,
    fmt::Debug,
    sync::atomic::{AtomicU32, Ordering},
};

use anyhow::{Context, Error};
use wasm_bindgen::{prelude::*, JsCast};
use wasmer::AsJs;

use crate::tasks::worker::{init_message_scheduler, WORKER_URL};
use crate::tasks::{
    AsyncJob, BlockingJob, PostMessagePayload, SchedulerMessage, WorkerHandle, WorkerMessage,
};

/// The scheduler instance.
pub static SCHEDULER: LazyLock<Scheduler> = LazyLock::new(Scheduler::spawn);

/// A handle for interacting with the threadpool's scheduler.
#[derive(Debug, Clone)]
pub(crate) struct Scheduler {}

thread_local! {
    /// Web worker instance that hosts the scheduler.
    ///
    /// This is only set in the main thread.
    static WORKER: OnceLock<web_sys::Worker> = OnceLock::new();
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
}

/// The state for the actor in charge of the threadpool.
#[derive(Debug)]
#[wasm_bindgen(skip_typescript)]
struct SchedulerState {
    /// Workers that are able to receive work.
    idle: VecDeque<WorkerHandle>,
    /// Workers that are currently blocked on synchronous operations and can't
    /// receive work at this time.
    busy: VecDeque<WorkerHandle>,
    /// Workers that are busy and cannot be reused afterwards.
    busy_non_reusable: VecDeque<WorkerHandle>,
}

#[wasm_bindgen]
impl SchedulerState {
    /// Runs the scheduler on this web worker.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        SchedulerState {
            idle: VecDeque::new(),
            busy: VecDeque::new(),
            busy_non_reusable: VecDeque::new(),
        }
    }

    /// Handles a web worker message
    #[wasm_bindgen]
    pub fn handle(&mut self, msg: JsValue) -> Result<(), utils::Error> {
        let msg = unsafe { SchedulerMessage::try_from_js(msg)? };

        tracing::debug!(?msg, "Executing a message");
        if let Err(e) = self.execute(msg) {
            tracing::error!(error = &*e, "An error occurred while handling a message");
        }

        Ok(())
    }

    fn execute(&mut self, message: SchedulerMessage) -> Result<(), Error> {
        match message {
            SchedulerMessage::SpawnAsync(task) => {
                self.post_message(PostMessagePayload::Async(AsyncJob::Thunk(task)))
            }
            SchedulerMessage::SpawnBlocking(task) => {
                self.post_message(PostMessagePayload::Blocking(BlockingJob::Thunk(task)))
            }
            SchedulerMessage::SpawnWithModule { module, task } => {
                self.post_message(PostMessagePayload::Blocking(BlockingJob::SpawnWithModule {
                    module,
                    task,
                }))
            }
            SchedulerMessage::SpawnWithModuleAndMemory {
                module,
                memory,
                spawn_wasm,
            } => {
                let temp_store = wasmer::Store::default();
                let memory = memory.map(|m| m.as_jsvalue(&temp_store).dyn_into().unwrap());

                self.post_message(PostMessagePayload::Blocking(
                    BlockingJob::SpawnWithModuleAndMemory {
                        module,
                        memory,
                        spawn_wasm,
                    },
                ))
            }
            SchedulerMessage::WorkerBusy { worker_id } => {
                move_worker(worker_id, &mut self.idle, &mut self.busy);
                tracing::trace!(
                    worker.id=worker_id,
                    idle_workers=?self.idle.iter().map(|w| w.id()).collect::<Vec<_>>(),
                    busy_workers=?self.busy.iter().map(|w| w.id()).collect::<Vec<_>>(),
                    "Worker marked as busy",
                );
                Ok(())
            }
            SchedulerMessage::WorkerIdle { worker_id } => {
                self.busy_non_reusable.retain(|w| w.id() != worker_id);
                move_worker(worker_id, &mut self.busy, &mut self.idle);
                tracing::trace!(
                    worker.id=worker_id,
                    idle_workers=?self.idle.iter().map(|w| w.id()).collect::<Vec<_>>(),
                    busy_workers=?self.busy.iter().map(|w| w.id()).collect::<Vec<_>>(),
                    "Worker marked as idle",
                );
                Ok(())
            }
            SchedulerMessage::Markers { uninhabited, .. } => match uninhabited {},
        }
    }

    /// Send a task to one of the worker threads, preferring workers that aren't
    /// running synchronous work.
    fn post_message(&mut self, msg: PostMessagePayload) -> Result<(), Error> {
        let reusable = msg.is_woker_reusable();
        let would_block = msg.would_block();

        let worker = self.next_available_worker()?;
        worker
            .send(msg)
            .with_context(|| format!("Unable to send a message to worker {}", worker.id()))?;

        if reusable {
            if would_block {
                self.busy.push_back(worker);
            } else {
                self.idle.push_back(worker);
            }
        } else {
            self.busy_non_reusable.push_back(worker);
        }

        Ok(())
    }

    fn next_available_worker(&mut self) -> Result<WorkerHandle, Error> {
        // First, try to send the message to an idle worker
        if let Some(worker) = self.idle.pop_front() {
            tracing::trace!(
                worker.id = worker.id(),
                "Sending the message to an idle worker"
            );
            return Ok(worker);
        }

        // Rather than sending the task to one of the blocking workers,
        // let's spawn a new worker

        let worker = self.start_worker()?;
        tracing::trace!(
            worker.id = worker.id(),
            "Sending the message to a new worker"
        );
        Ok(worker)
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

fn move_worker(worker_id: u32, from: &mut VecDeque<WorkerHandle>, to: &mut VecDeque<WorkerHandle>) {
    if let Some(ix) = from.iter().position(|w| w.id() == worker_id) {
        let worker = from.remove(ix).unwrap();
        to.push_back(worker);
    }
}
