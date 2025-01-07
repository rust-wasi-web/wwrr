use std::fmt::Debug;

use anyhow::Error;
use wasm_bindgen::{prelude::*, JsCast};

use super::scheduler::Scheduler;
use super::worker::{init_message_worker, WORKER_URL};
use crate::tasks::PostMessagePayload;

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
    pub(crate) fn spawn(worker_id: u32, scheduler: Scheduler) -> Result<Self, Error> {
        let name = format!("worker-{worker_id}");

        let wo = web_sys::WorkerOptions::new();
        wo.set_name(&name);
        wo.set_type(web_sys::WorkerType::Module);

        let worker =
            web_sys::Worker::new_with_options(&WORKER_URL, &wo).map_err(utils::js_error)?;

        let on_error: Closure<dyn FnMut(web_sys::ErrorEvent)> =
            Closure::new(move |msg| on_error(msg, worker_id));
        let on_error: js_sys::Function = on_error.into_js_value().unchecked_into();
        worker.set_onerror(Some(&on_error));

        // The worker has technically been started, but it's kinda useless
        // because it hasn't been initialized with the same WebAssembly module
        // and linear memory as the scheduler. We need to initialize explicitly.
        let init_msg = init_message_worker(worker_id);
        worker.post_message(&init_msg).unwrap();

        // Provide channel to scheduler to worker.
        let init_worker_msg = PostMessagePayload::Init { scheduler };
        worker
            .post_message(&init_worker_msg.into_js().unwrap())
            .unwrap();

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

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        tracing::trace!(id = self.id(), "Terminating worker");
        self.inner.terminate();
    }
}
