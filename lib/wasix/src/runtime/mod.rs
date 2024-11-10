pub mod module_cache;
pub mod task_manager;

pub use self::task_manager::{SpawnMemoryType, VirtualTaskManager};
use self::{module_cache::CacheError, task_manager::InlineWaker};
use wasmer_types::ModuleHash;

use std::{
    fmt,
    sync::{Arc, Mutex},
};

use futures::future::BoxFuture;
use virtual_net::DynVirtualNetworking;
use wasmer::{Module, RuntimeError};
use wasmer_wasix_types::wasi::ExitCode;

use crate::{
    http::DynHttpClient,
    os::TtyBridge,
    runtime::module_cache::{ModuleCache, ThreadLocalCache},
    SpawnError, WasiTtyState,
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
#[allow(unused_variables)]
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

    /// Get a custom HTTP client
    fn http_client(&self) -> Option<&DynHttpClient> {
        None
    }

    /// Get access to the TTY used by the environment.
    fn tty(&self) -> Option<&(dyn TtyBridge + Send + Sync)> {
        None
    }

    /// Load a a Webassembly module, trying to use a pre-compiled version if possible.
    fn load_module<'a>(&'a self, wasm: &'a [u8]) -> BoxFuture<'a, Result<Module, SpawnError>> {
        let engine = self.engine();
        let module_cache = self.module_cache();
        let hash = ModuleHash::xxhash(wasm);

        let task = async move { load_module(&engine, &module_cache, wasm, hash).await };

        Box::pin(task)
    }

    /// Load a a Webassembly module, trying to use a pre-compiled version if possible.
    ///
    /// Non-async version of [`Self::load_module`].
    fn load_module_sync(&self, wasm: &[u8]) -> Result<Module, SpawnError> {
        InlineWaker::block_on(self.load_module(wasm))
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
#[tracing::instrument(level = "debug", skip_all)]
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

    let module = Module::new(&engine, wasm).map_err(|err| crate::SpawnError::CompileError {
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

#[derive(Debug, Default)]
pub struct DefaultTty {
    state: Mutex<WasiTtyState>,
}

impl TtyBridge for DefaultTty {
    fn reset(&self) {
        let mut state = self.state.lock().unwrap();
        state.echo = false;
        state.line_buffered = false;
        state.line_feeds = false
    }

    fn tty_get(&self) -> WasiTtyState {
        let state = self.state.lock().unwrap();
        state.clone()
    }

    fn tty_set(&self, tty_state: WasiTtyState) {
        let mut state = self.state.lock().unwrap();
        *state = tty_state;
    }
}
