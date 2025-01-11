use std::fmt::Debug;
use std::marker::PhantomData;

use anyhow::anyhow;
use anyhow::Error;
use tokio::sync::{mpsc, oneshot};
use wasm_bindgen::{prelude::*, JsCast};

use super::scheduler::Scheduler;
use super::worker::{init_message_worker, WORKER_URL};
use super::worker_message::WorkerMsg;
use crate::tasks::WorkerInit;

/// A handle to a running [`web_sys::Worker`].
///
/// This provides a structured way to communicate with the worker and will
/// automatically call [`web_sys::Worker::terminate()`] when dropped.
#[derive(Debug)]
pub(crate) struct WorkerHandle {
    id: u32,
    web_worker: web_sys::Worker,
    msg_tx: mpsc::UnboundedSender<WorkerMsg>,
    terminate: bool,
}

impl WorkerHandle {
    /// Spawns a new web worker.
    pub(crate) async fn spawn(
        id: u32,
        scheduler: Scheduler,
        module: wasmer::Module,
        memory: wasmer::Memory,
        wbg_js_module_name: String,
    ) -> Result<Self, Error> {
        let name = format!("worker-{id}");

        let wo = web_sys::WorkerOptions::new();
        wo.set_name(&name);
        wo.set_type(web_sys::WorkerType::Module);

        let web_worker =
            web_sys::Worker::new_with_options(&WORKER_URL, &wo).map_err(utils::js_error)?;

        let on_error: Closure<dyn FnMut(web_sys::ErrorEvent)> =
            Closure::new(move |msg| on_error(msg, id));
        let on_error: js_sys::Function = on_error.into_js_value().unchecked_into();
        web_worker.set_onerror(Some(&on_error));

        // The worker has technically been started, but it's kinda useless
        // because it hasn't been initialized with the same WebAssembly module
        // and linear memory as the scheduler. We need to initialize explicitly.
        let init_msg = init_message_worker(id);
        web_worker.post_message(&init_msg).unwrap();

        // Provide initialization data to worker.
        let (ready_tx, ready_rx) = oneshot::channel();
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let init_worker_msg = WorkerInit {
            scheduler,
            ready_tx,
            msg_rx,
            module,
            memory,
            wbg_js_module_name,
            _not_send: PhantomData,
        };
        web_worker
            .post_message(&init_worker_msg.into_js().unwrap())
            .unwrap();

        // Wait for worker to initialize.
        tracing::trace!("waiting for worker {id}");
        ready_rx.await.unwrap();
        tracing::trace!("worker {id} is ready");

        Ok(WorkerHandle {
            id,
            web_worker,
            msg_tx,
            terminate: true,
        })
    }

    /// The id of the web worker.
    pub(crate) fn id(&self) -> u32 {
        self.id
    }

    /// Send a message to the worker.
    pub(crate) fn send(&self, msg: WorkerMsg) -> Result<(), Error> {
        tracing::trace!(?msg, worker.id = self.id(), "sending a message to a worker");
        self.msg_tx.send(msg).map_err(|_| anyhow!("worker died"))?;
        Ok(())
    }

    /// Sets whether the worker is terminated when this handle is dropped.
    pub(crate) fn set_terminate(&mut self, terminate: bool) {
        self.terminate = terminate;
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
        if self.terminate {
            tracing::trace!(worker.id = self.id(), "Terminating worker");
            self.web_worker.terminate();
        }
    }
}
