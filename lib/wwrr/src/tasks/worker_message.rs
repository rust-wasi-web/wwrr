use utils::Error;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::DedicatedWorkerGlobalScope;

use crate::tasks::{
    interop::{Deserializer, Serializer},
    SchedulerMessage,
};

/// A message the worker sends back to the scheduler.
#[derive(Debug)]
pub(crate) enum WorkerMessage {
    /// Mark this worker as done.
    Done,
    /// Message to scheduler.
    Scheduler(SchedulerMessage),
}

impl WorkerMessage {
    pub(crate) unsafe fn try_from_js(value: JsValue) -> Result<Self, Error> {
        let de = Deserializer::new(value);

        match de.ty()?.as_str() {
            consts::TYPE_DONE => Ok(WorkerMessage::Done),
            consts::TYPE_SCHEDULER => {
                let value: JsValue = de.js(consts::MESSAGE)?;
                let msg = SchedulerMessage::try_from_js(value)?;
                Ok(WorkerMessage::Scheduler(msg))
            }
            other => Err(anyhow::anyhow!("Unknown message type, \"{other}\"").into()),
        }
    }

    pub(crate) fn into_js(self) -> Result<JsValue, Error> {
        match self {
            WorkerMessage::Done => Serializer::new(consts::TYPE_DONE).finish(),
            WorkerMessage::Scheduler(msg) => {
                let msg = msg.into_js()?;
                Serializer::new(consts::TYPE_SCHEDULER)
                    .set(consts::MESSAGE, msg)
                    .finish()
            }
        }
    }

    /// Send this message to the scheduler.
    pub fn emit(self) -> Result<(), Error> {
        tracing::debug!(
            current_thread = wasmer::current_thread_id(),
            msg=?self,
            "Sending a worker message"
        );
        let scope: DedicatedWorkerGlobalScope = js_sys::global()
            .dyn_into()
            .expect("Should only ever be executed from a worker");

        let value = self.into_js()?;
        scope.post_message(&value).map_err(Error::js)?;

        Ok(())
    }
}

mod consts {
    pub const TYPE_DONE: &str = "done";
    pub const TYPE_SCHEDULER: &str = "scheduler";
    pub const MESSAGE: &str = "msg";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_done() {
        let msg = WorkerMessage::Done;

        let js = msg.into_js().unwrap();
        let round_tripped = unsafe { WorkerMessage::try_from_js(js).unwrap() };

        assert!(matches!(round_tripped, WorkerMessage::Done));
    }

    #[test]
    fn round_trip_scheduler_message() {
        let msg = WorkerMessage::Scheduler(SchedulerMessage::WorkerDone { worker_id: 42 });

        let js = msg.into_js().unwrap();
        let round_tripped = unsafe { WorkerMessage::try_from_js(js).unwrap() };

        assert!(matches!(
            round_tripped,
            WorkerMessage::Scheduler(SchedulerMessage::WorkerDone { worker_id: 42 })
        ));
    }
}
