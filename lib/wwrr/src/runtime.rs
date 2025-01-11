use std::sync::{Arc, LazyLock, Mutex, Weak};

use utils::Error;
use virtual_net::VirtualNetworking;
use wasmer::VERSION;
use wasmer_wasix::{runtime::module_cache::ThreadLocalCache, VirtualTaskManager};

use crate::tasks::ThreadPool;

/// Worker thread pool.
static THREAD_POOL: LazyLock<Arc<dyn VirtualTaskManager>> =
    LazyLock::new(|| Arc::new(ThreadPool::new()));

/// A weak reference to the global [`Runtime`].
static GLOBAL_RUNTIME: Mutex<Weak<Runtime>> = Mutex::new(Weak::new());

/// Runtime components used when running WebAssembly programs.
#[derive(Clone, derivative::Derivative)]
#[derivative(Debug)]
pub struct Runtime {
    networking: Arc<dyn VirtualNetworking>,
    module_cache: Arc<ThreadLocalCache>,
}

impl Runtime {
    /// Get a reference to the global runtime, if it has already been
    /// initialized.
    pub(crate) fn global() -> Option<Arc<Runtime>> {
        GLOBAL_RUNTIME.lock().ok()?.upgrade()
    }

    /// Get a reference to the global runtime, initializing it if it hasn't
    /// already been.
    pub(crate) fn lazily_initialized() -> Result<Arc<Self>, Error> {
        match GLOBAL_RUNTIME.lock() {
            Ok(mut guard) => match guard.upgrade() {
                Some(rt) => Ok(rt),
                None => {
                    tracing::info!("WWRR - WASI Web Reactor Runtime - version {VERSION}");
                    let rt = Arc::new(Runtime::with_defaults()?);
                    *guard = Arc::downgrade(&rt);

                    Ok(rt)
                }
            },
            Err(mut e) => {
                tracing::warn!("The global runtime lock was poisoned. Reinitializing.");

                let rt = Arc::new(Runtime::with_defaults()?);
                **e.get_mut() = Arc::downgrade(&rt);

                // FIXME: Use this when it becomes stable
                // GLOBAL_RUNTIME.clear_poison();

                Ok(rt)
            }
        }
    }

    pub(crate) fn with_defaults() -> Result<Self, Error> {
        let rt = Runtime::new();
        Ok(rt)
    }

    pub(crate) fn new() -> Self {
        Runtime {
            networking: Arc::new(virtual_net::UnsupportedVirtualNetworking::default()),
            module_cache: Arc::new(ThreadLocalCache::default()),
        }
    }
}

impl wasmer_wasix::runtime::Runtime for Runtime {
    fn networking(&self) -> &Arc<dyn VirtualNetworking> {
        &self.networking
    }

    fn task_manager(&self) -> &Arc<dyn VirtualTaskManager> {
        &*THREAD_POOL
    }

    fn module_cache(
        &self,
    ) -> Arc<dyn wasmer_wasix::runtime::module_cache::ModuleCache + Send + Sync> {
        self.module_cache.clone()
    }
}
