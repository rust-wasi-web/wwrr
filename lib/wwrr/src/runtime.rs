use std::sync::{Arc, Mutex, Weak};

use lazy_static::lazy_static;
use once_cell::sync::Lazy;
use virtual_net::VirtualNetworking;
use wasmer_wasix::{runtime::module_cache::ThreadLocalCache, VirtualTaskManager};

lazy_static! {
    /// We initialize the ThreadPool lazily
    static ref DEFAULT_THREAD_POOL: Arc<ThreadPool> = Arc::new(ThreadPool::new());
}

use crate::{tasks::ThreadPool, utils::Error};

/// A weak reference to the global [`Runtime`].
static GLOBAL_RUNTIME: Lazy<Mutex<Weak<Runtime>>> = Lazy::new(Mutex::default);

/// Runtime components used when running WebAssembly programs.
#[derive(Clone, derivative::Derivative)]
#[derivative(Debug)]
pub struct Runtime {
    task_manager: Option<Arc<dyn VirtualTaskManager>>,
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
                    tracing::debug!("Initializing the global runtime");
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

    pub(crate) fn with_task_manager(&self, task_manager: Arc<ThreadPool>) -> Self {
        let mut runtime = self.clone();
        runtime.task_manager = Some(task_manager);
        runtime
    }

    pub(crate) fn with_default_pool(&self) -> Self {
        self.with_task_manager(DEFAULT_THREAD_POOL.clone())
    }

    pub(crate) fn new() -> Self {
        Runtime {
            task_manager: None,
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
        &self.task_manager.as_ref().expect("Task manager not found")
    }

    fn module_cache(
        &self,
    ) -> Arc<dyn wasmer_wasix::runtime::module_cache::ModuleCache + Send + Sync> {
        self.module_cache.clone()
    }
}

#[cfg(test)]
mod tests {
    use wasm_bindgen_test::wasm_bindgen_test;
    use wasmer::Module;
    use wasmer_wasix::{Runtime as _, WasiEnvBuilder};

    use super::*;

    pub(crate) const TRIVIAL_WAT: &[u8] = br#"(
        module
            (memory $memory 0)
            (export "memory" (memory $memory))
            (func (export "_start") nop)
        )"#;

    #[wasm_bindgen_test]
    async fn execute_a_trivial_module() {
        let runtime = Runtime::with_defaults().unwrap().with_default_pool();
        let module = Module::new(TRIVIAL_WAT).await.unwrap();

        WasiEnvBuilder::new("trivial")
            .runtime(Arc::new(runtime))
            .run(module)
            .unwrap();
    }
}
