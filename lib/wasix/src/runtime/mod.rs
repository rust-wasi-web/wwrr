pub mod module_cache;
pub mod task_manager;

use self::module_cache::CacheError;
pub use self::task_manager::{SpawnMemoryType, VirtualTaskManager};
use wasmer_types::ModuleHash;

use std::{fmt, sync::Arc};

use futures::future::LocalBoxFuture;
use virtual_net::DynVirtualNetworking;
use wasmer::{Module, RuntimeError};
use wasmer_wasix_types::wasi::ExitCode;

use crate::{
    runtime::module_cache::{ModuleCache, ThreadLocalCache},
    SpawnError,
};

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

    /// A cache for compiled modules.
    fn module_cache(&self) -> Arc<dyn ModuleCache + Send + Sync> {
        // Return a cache that uses a thread-local variable. This isn't ideal
        // because it allows silently sharing state, possibly between runtimes.
        //
        // That said, it means people will still get *some* level of caching
        // because each cache returned by this default implementation will go
        // through the same thread-local variable.
        Arc::new(ThreadLocalCache::default())
    }

    /// Get a [`wasmer::Engine`] for module compilation.
    fn engine(&self) -> wasmer::Engine {
        wasmer::Engine::default()
    }

    /// Create a new [`wasmer::Store`].
    fn new_store(&self) -> wasmer::Store {
        wasmer::Store::default()
    }

    /// Load a a Webassembly module, trying to use a pre-compiled version if possible.
    fn load_module<'a>(&'a self, wasm: &'a [u8]) -> LocalBoxFuture<'a, Result<Module, SpawnError>> {
        let engine = self.engine();
        let module_cache = self.module_cache();
        let hash = ModuleHash::xxhash(wasm);

        let task = async move { load_module(&engine, &module_cache, wasm, hash).await };

        Box::pin(task)
    }

    /// Callback thats invokes whenever the instance is tainted, tainting can occur
    /// for multiple reasons however the most common is a panic within the process
    fn on_taint(&self, _reason: TaintReason) {}
}

pub type DynRuntime = dyn Runtime + Send + Sync;

/// Load a a Webassembly module, trying to use a pre-compiled version if possible.
///
// This function exists to provide a reusable baseline implementation for
// implementing [`Runtime::load_module`], so custom logic can be added on top.
#[tracing::instrument(level = "trace", skip_all)]
pub async fn load_module(
    engine: &wasmer::Engine,
    module_cache: &(dyn ModuleCache + Send + Sync),
    wasm: &[u8],
    wasm_hash: ModuleHash,
) -> Result<Module, crate::SpawnError> {
    let result = module_cache.load(wasm_hash, engine).await;

    match result {
        Ok(module) => return Ok(module),
        Err(CacheError::NotFound) => {}
        Err(other) => {
            tracing::warn!(
                %wasm_hash,
                error=&other as &dyn std::error::Error,
                "Unable to load the cached module",
            );
        }
    }

    let module = Module::new(wasm)
        .await
        .map_err(|err| crate::SpawnError::CompileError {
            module_hash: wasm_hash,
            error: err,
        })?;

    if let Err(e) = module_cache.save(wasm_hash, engine, &module).await {
        tracing::warn!(
            %wasm_hash,
            error=&e as &dyn std::error::Error,
            "Unable to cache the compiled module",
        );
    }

    Ok(module)
}
