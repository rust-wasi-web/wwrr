use std::{
    collections::VecDeque,
    fmt::Debug,
    sync::atomic::{AtomicU32, Ordering},
};

use anyhow::{Context, Error};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::{self};
use tracing::Instrument;
use wasm_bindgen::JsCast;
use wasmer::AsJs;

use crate::tasks::{
    AsyncJob, BlockingJob, PostMessagePayload, SchedulerMessage, WorkerHandle, WorkerMessage,
};

/// A handle for interacting with the threadpool's scheduler.
#[derive(Debug, Clone)]
pub(crate) struct Scheduler {
    scheduler_thread_id: u32,
    channel: UnboundedSender<SchedulerMessage>,
}

impl Scheduler {
    /// Spin up a scheduler on the current thread and get a channel that can be
    /// used to communicate with it.
    pub(crate) fn spawn() -> Scheduler {
        let (sender, mut receiver) = mpsc::unbounded_channel();

        let thread_id = wasmer::current_thread_id();
        // Safety: we just got the thread ID.
        let sender = unsafe { Scheduler::new(sender, thread_id) };

        let mut scheduler = SchedulerState::new(sender.clone());

        tracing::debug!(thread_id, "Spinning up the scheduler");
        wasm_bindgen_futures::spawn_local(
            async move {
                while let Some(msg) = receiver.recv().await {
                    tracing::debug!(?msg, "Executing a message");

                    if let Err(e) = scheduler.execute(msg) {
                        tracing::error!(error = &*e, "An error occurred while handling a message");
                    }
                }

                tracing::debug!("Shutting down the scheduler");
                drop(scheduler);
            }
            .in_current_span()
            .instrument(tracing::debug_span!("scheduler", thread_id = thread_id)),
        );

        sender
    }

    /// # Safety
    ///
    /// The [`SchedulerMessage`] type is marked as `!Send` because
    /// [`wasmer::Module`] and friends are `!Send` when compiled for the
    /// browser.
    ///
    /// The `scheduler_thread_id` must match the [`wasmer::current_thread_id()`]
    /// otherwise these `!Send` values will be sent between threads.
    unsafe fn new(channel: UnboundedSender<SchedulerMessage>, scheduler_thread_id: u32) -> Self {
        debug_assert_eq!(scheduler_thread_id, wasmer::current_thread_id());
        Scheduler {
            channel,
            scheduler_thread_id,
        }
    }

    pub fn send(&self, msg: SchedulerMessage) -> Result<(), Error> {
        if wasmer::current_thread_id() == self.scheduler_thread_id {
            tracing::debug!(
                current_thread = wasmer::current_thread_id(),
                ?msg,
                "Sending message to scheduler"
            );
            // It's safe to send the message to the scheduler.
            self.channel
                .send(msg)
                .map_err(|_| Error::msg("Scheduler is dead"))?;
            Ok(())
        } else {
            // We are in a child worker so we need to emit the message via
            // postMessage() and let the WorkerHandle forward it to the
            // scheduler.
            WorkerMessage::Scheduler(msg)
                .emit()
                .map_err(|e| e.into_anyhow())?;
            Ok(())
        }
    }
}

// Safety: The only way our !Send messages will be sent to the scheduler is if
// they are on the same thread. This is enforced via Scheduler::new()'s
// invariants.
unsafe impl Send for Scheduler {}
unsafe impl Sync for Scheduler {}

/// The state for the actor in charge of the threadpool.
#[derive(Debug)]
struct SchedulerState {
    /// Workers that are able to receive work.
    idle: VecDeque<WorkerHandle>,
    /// Workers that are currently blocked on synchronous operations and can't
    /// receive work at this time.
    busy: VecDeque<WorkerHandle>,
    /// Workers that are busy and cannot be reused afterwards.
    busy_non_reusable: VecDeque<WorkerHandle>,
    /// A channel that can be used to send messages to this scheduler.
    mailbox: Scheduler,
}

impl SchedulerState {
    fn new(mailbox: Scheduler) -> Self {
        SchedulerState {
            idle: VecDeque::new(),
            busy: VecDeque::new(),
            busy_non_reusable: VecDeque::new(),
            mailbox,
        }
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

        let handle = WorkerHandle::spawn(id, self.mailbox.clone())?;
        Ok(handle)
    }
}

fn move_worker(worker_id: u32, from: &mut VecDeque<WorkerHandle>, to: &mut VecDeque<WorkerHandle>) {
    if let Some(ix) = from.iter().position(|w| w.id() == worker_id) {
        let worker = from.remove(ix).unwrap();
        to.push_back(worker);
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::oneshot;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    #[wasm_bindgen_test]
    async fn spawn_an_async_function() {
        let (sender, receiver) = oneshot::channel();
        let (tx, _) = mpsc::unbounded_channel();
        let tx = unsafe { Scheduler::new(tx, wasmer::current_thread_id()) };
        let mut scheduler = SchedulerState::new(tx);
        let message = SchedulerMessage::SpawnAsync(Box::new(move || {
            Box::pin(async move {
                let _ = sender.send(42);
            })
        }));

        // we start off with no workers
        assert_eq!(scheduler.idle.len(), 0);
        assert_eq!(scheduler.busy.len(), 0);

        // then we run the message, which should start up a worker and send it
        // the job
        scheduler.execute(message).unwrap();

        // One worker should have been created and added to the "ready" queue
        // because it's just handling async workloads.
        assert_eq!(scheduler.idle.len(), 1);
        assert_eq!(scheduler.busy.len(), 0);

        // Make sure the background thread actually ran something and sent us
        // back a result
        assert_eq!(receiver.await.unwrap(), 42);
    }
}
