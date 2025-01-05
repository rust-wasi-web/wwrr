use std::fmt::Debug;

use anyhow::{Context, Error};
use wasm_bindgen::{prelude::*, JsCast};

use super::worker::{init_message_worker, WORKER_URL};
use crate::tasks::{PostMessagePayload, SchedulerMessage, WorkerMessage, SCHEDULER};

/// A handle to a running [`web_sys::Worker`].
///
/// This provides a structured way to communicate with the worker and will
/// automatically call [`web_sys::Worker::terminate()`] when dropped.
#[derive(Debug)]
pub(crate) struct WorkerHandle {
    id: u32,
    inner: web_sys::Worker,
}

impl WorkerHandle {
    /// Spawns a new web worker.
    pub(crate) fn spawn(worker_id: u32) -> Result<Self, Error> {
        let name = format!("worker-{worker_id}");

        let wo = web_sys::WorkerOptions::new();
        wo.set_name(&name);
        wo.set_type(web_sys::WorkerType::Module);

        let worker =
            web_sys::Worker::new_with_options(&WORKER_URL, &wo).map_err(utils::js_error)?;

        let on_message: Closure<dyn FnMut(web_sys::MessageEvent)> =
            Closure::new(move |msg: web_sys::MessageEvent| on_message(msg, worker_id));
        let on_message: js_sys::Function = on_message.into_js_value().unchecked_into();
        worker.set_onmessage(Some(&on_message));

        let on_error: Closure<dyn FnMut(web_sys::ErrorEvent)> =
            Closure::new(move |msg| on_error(msg, worker_id));
        let on_error: js_sys::Function = on_error.into_js_value().unchecked_into();
        worker.set_onerror(Some(&on_error));

        // The worker has technically been started, but it's kinda useless
        // because it hasn't been initialized with the same WebAssembly module
        // and linear memory as the scheduler. We need to initialize explicitly.
        let init_msg = init_message_worker(worker_id);
        worker.post_message(&init_msg).unwrap();

        Ok(WorkerHandle {
            id: worker_id,
            inner: worker,
        })
    }

    /// The id of the web worker.
    pub(crate) fn id(&self) -> u32 {
        self.id
    }

    /// Send a message to the worker.
    pub(crate) fn send(&self, msg: PostMessagePayload) -> Result<(), Error> {
        tracing::trace!(?msg, worker.id = self.id(), "sending a message to a worker");
        let js = msg.into_js().map_err(|e| e.into_anyhow())?;

        self.inner.post_message(&js).map_err(utils::js_error)?;

        Ok(())
    }
}

#[tracing::instrument(level = "trace", skip_all, fields(worker.id=worker_id))]
fn on_error(msg: web_sys::ErrorEvent, worker_id: u32) {
    tracing::error!(
        error = %msg.message(),
        filename = %msg.filename(),
        line_number = %msg.lineno(),
        column = %msg.colno(),
        "An error occurred",
    );
}

#[tracing::instrument(level = "trace", skip_all, fields(worker.id=worker_id))]
fn on_message(msg: web_sys::MessageEvent, worker_id: u32) {
    // Safety: The only way we can receive this message is if it was from the
    // worker, because we are the ones that spawned the worker, we can trust
    // the messages it emits.
    let result = unsafe { WorkerMessage::try_from_js(msg.data()) }
        .map_err(|e| utils::js_error(e.into()))
        .context("Unable to parse the worker message")
        .and_then(|msg| {
            tracing::trace!(
                ?msg,
                worker.id = worker_id,
                "Received a message from worker"
            );

            let msg = match msg {
                WorkerMessage::MarkBusy => SchedulerMessage::WorkerBusy { worker_id },
                WorkerMessage::MarkIdle => SchedulerMessage::WorkerIdle { worker_id },
                WorkerMessage::Scheduler(msg) => msg,
            };
            SCHEDULER.send(msg).map_err(|_| Error::msg("Send failed"))
        });

    if let Err(e) = result {
        tracing::warn!(
            error = &*e,
            msg.origin = msg.origin(),
            msg.last_event_id = msg.last_event_id(),
            "Unable to handle a message from the worker",
        );
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        tracing::trace!(id = self.id(), "Terminating worker");
        self.inner.terminate();
    }
}
