pub mod task_manager;

use std::{fmt, sync::Arc};

use bytes::Bytes;
use futures::future::LocalBoxFuture;
use virtual_net::DynVirtualNetworking;
use wasmer::{Module, RuntimeError};
use wasmer_types::ModuleHash;
use wasmer_wasix_types::wasi::ExitCode;

pub use self::task_manager::{SpawnMemoryType, VirtualTaskManager};

use crate::SpawnError;

#[derive(Clone)]
pub enum TaintReason {
    UnknownWasiVersion,
    NonZeroExitCode(ExitCode),
    RuntimeError(RuntimeError),
}

/// Runtime components used when running WebAssembly programs.
///
/// Think of this as the "System" in "WebAssembly Systems Interface".
pub trait Runtime
where
    Self: fmt::Debug,
{
    /// Provides access to all the networking related functions such as sockets.
    fn networking(&self) -> &DynVirtualNetworking;

    /// Retrieve the active [`VirtualTaskManager`].
    fn task_manager(&self) -> &Arc<dyn VirtualTaskManager>;

    /// Get a [`wasmer::Engine`] for module compilation.
    fn engine(&self) -> wasmer::Engine {
        wasmer::Engine::default()
    }

    /// Create a new [`wasmer::Store`].
    fn new_store(&self) -> wasmer::Store {
        wasmer::Store::default()
    }

    /// Load a a Webassembly module.
    fn load_module<'a>(&'a self, wasm: &'a [u8]) -> LocalBoxFuture<'a, Result<Module, SpawnError>> {
        let engine = self.engine();
        let hash = ModuleHash::xxhash(wasm);

        let task = async move { load_module(&engine, wasm, hash).await };

        Box::pin(task)
    }

    /// Callback thats invokes whenever the instance is tainted, tainting can occur
    /// for multiple reasons however the most common is a panic within the process
    fn on_taint(&self, _reason: TaintReason) {}
}

pub type DynRuntime = dyn Runtime + Send + Sync;

/// Load a a Webassembly module.
///
// This function exists to provide a reusable baseline implementation for
// implementing [`Runtime::load_module`], so custom logic can be added on top.
#[tracing::instrument(level = "trace", skip_all)]
pub async fn load_module(
    _engine: &wasmer::Engine,
    wasm: &[u8],
    wasm_hash: ModuleHash,
) -> Result<Module, crate::SpawnError> {
    let module = Module::from_binary(Bytes::from(wasm.to_vec()))
        .await
        .map_err(|err| crate::SpawnError::CompileError {
            module_hash: wasm_hash,
            error: err,
        })?;

    Ok(module)
}
