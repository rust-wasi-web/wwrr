use std::ops::Deref;
use std::{pin::Pin, time::Duration};

use derivative::Derivative;
use futures::future::LocalBoxFuture;
use futures::Future;
use wasm_bindgen::JsValue;
use wasmer::{Memory, MemoryType, Module, Store, StoreMut, StoreRef};

use crate::os::task::thread::WasiThreadError;
use crate::{StoreSnapshot, WasiEnv, WasiFunctionEnv};

pub use virtual_mio::InlineWaker;

#[derive(Debug)]
pub enum SpawnMemoryType<'a> {
    CreateMemory,
    CreateMemoryOfType(MemoryType),
    // TODO: is there a way to get rid of the memory reference
    ShareMemory(Memory, StoreRef<'a>),
    // TODO: is there a way to get rid of the memory reference
    // Note: The message sender is triggered once the memory
    // has been copied, this makes sure its not modified until
    // its been properly copied
    CopyMemory(Memory, StoreRef<'a>),
}

/// The properties passed to the task
#[derive(Derivative)]
#[derivative(Debug)]
pub struct TaskWasmRunProperties {
    pub ctx: WasiFunctionEnv,
    pub store: Store,
    /// wasm-bindgen generated JavaScript module.
    pub wbg_js_module: Option<JsValue>,
}

/// Async callback that will be invoked
pub type TaskWasmRun = dyn FnOnce(TaskWasmRunProperties) -> LocalBoxFuture<'static, anyhow::Result<()>>
    + Send
    + 'static;

/// Represents a WASM task that will be executed on a dedicated thread
pub struct TaskWasm<'a, 'b> {
    pub run: Box<TaskWasmRun>,
    pub env: WasiEnv,
    pub module: Module,
    pub globals: Option<&'b StoreSnapshot>,
    pub spawn_type: SpawnMemoryType<'a>,
}

impl<'a, 'b> TaskWasm<'a, 'b> {
    pub fn new(run: Box<TaskWasmRun>, env: WasiEnv, module: Module) -> Self {
        let shared_memory = module.imports().memories().next().map(|a| *a.ty());
        Self {
            run,
            env,
            module,
            globals: None,
            spawn_type: match shared_memory {
                Some(ty) => SpawnMemoryType::CreateMemoryOfType(ty),
                None => SpawnMemoryType::CreateMemory,
            },
        }
    }

    pub fn with_memory(mut self, spawn_type: SpawnMemoryType<'a>) -> Self {
        self.spawn_type = spawn_type;
        self
    }

    pub fn with_optional_memory(mut self, spawn_type: Option<SpawnMemoryType<'a>>) -> Self {
        if let Some(spawn_type) = spawn_type {
            self.spawn_type = spawn_type;
        }
        self
    }

    pub fn with_globals(mut self, snapshot: &'b StoreSnapshot) -> Self {
        self.globals.replace(snapshot);
        self
    }
}

/// Data for spawning the scheduler.
#[derive(Debug)]
pub struct SchedulerSpawn {
    /// WebAssembly module for spawning threads.
    pub module: Module,
    /// WebAssembly memory for spawning threads.
    pub memory: Memory,
    /// wasm-bindgen generated module name.
    pub wbg_js_module_name: String,
    /// Number of workers to pre-start.
    pub prestarted_workers: usize,
}

/// A task executor backed by a thread pool.
///
/// ## Thread Safety
///
/// Due to [#4158], it is possible to pass non-thread safe objects across
/// threads by capturing them in the task passed to
/// [`VirtualTaskManager::task_shared()`] or
/// [`VirtualTaskManager::task_dedicated()`].
///
/// If your task needs access to a [`wasmer::Module`], [`wasmer::Memory`], or
/// [`wasmer::Instance`], it should explicitly transfer the objects using
/// either [`VirtualTaskManager::task_wasm()`] when in syscall context or
/// [`VirtualTaskManager::spawn_with_module()`] for higher level code.
///
/// [#4158]: https://github.com/wasmerio/wasmer/issues/4158
pub trait VirtualTaskManager: std::fmt::Debug + Send + Sync + 'static {
    /// Initializes the task manager.
    fn init(&self, scheduler_spawn: SchedulerSpawn) -> LocalBoxFuture<()>;

    /// Build a new Webassembly memory.
    ///
    /// May return `None` if the memory can just be auto-constructed.
    fn build_memory(
        &self,
        mut store: &mut StoreMut,
        spawn_type: SpawnMemoryType,
    ) -> Result<Option<Memory>, WasiThreadError> {
        match spawn_type {
            SpawnMemoryType::CreateMemoryOfType(mut ty) => {
                ty.shared = true;

                // Note: If memory is shared, maximum needs to be set in the
                // browser otherwise creation will fail.
                let _ = ty.maximum.get_or_insert(wasmer_types::Pages::max_value());

                let mem = Memory::new(&mut store, ty).map_err(|err| {
                    tracing::error!(
                        error = &err as &dyn std::error::Error,
                        memory_type=?ty,
                        "could not create memory",
                    );
                    WasiThreadError::MemoryCreateFailed(err)
                })?;
                Ok(Some(mem))
            }
            SpawnMemoryType::ShareMemory(mem, old_store) => {
                let mem = mem.share_in_store(&old_store, store).map_err(|err| {
                    tracing::warn!(
                        error = &err as &dyn std::error::Error,
                        "could not clone memory",
                    );
                    WasiThreadError::MemoryCreateFailed(err)
                })?;
                Ok(Some(mem))
            }
            SpawnMemoryType::CopyMemory(mem, old_store) => {
                let mem = mem.copy_to_store(&old_store, store).map_err(|err| {
                    tracing::warn!(
                        error = &err as &dyn std::error::Error,
                        "could not copy memory",
                    );
                    WasiThreadError::MemoryCreateFailed(err)
                })?;
                Ok(Some(mem))
            }
            SpawnMemoryType::CreateMemory => Ok(None),
        }
    }

    /// Pause the current thread of execution.
    ///
    /// This is typically invoked whenever a WASM thread goes idle. Besides
    /// acting as a platform-agnostic [`std::thread::sleep()`], this also gives
    /// the runtime a chance to do asynchronous work like pumping an event
    /// loop.
    fn sleep_now(
        &self,
        time: Duration,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>>;

    /// Run a WebAssembly operation on the thread pool.
    ///
    /// This is primarily used inside the context of a syscall and allows
    /// the transfer of things like [`wasmer::Module`] across threads.
    fn task_wasm(&self, task: TaskWasm) -> Result<(), WasiThreadError>;

    /// Returns the amount of parallelism that is possible on this platform.
    fn thread_parallelism(&self) -> Result<usize, WasiThreadError>;
}

impl<D, T> VirtualTaskManager for D
where
    D: Deref<Target = T> + std::fmt::Debug + Send + Sync + 'static,
    T: VirtualTaskManager + ?Sized,
{
    fn init(&self, scheduler_spawn: SchedulerSpawn) -> LocalBoxFuture<()> {
        (**self).init(scheduler_spawn)
    }

    fn build_memory(
        &self,
        store: &mut StoreMut,
        spawn_type: SpawnMemoryType,
    ) -> Result<Option<Memory>, WasiThreadError> {
        (**self).build_memory(store, spawn_type)
    }

    fn sleep_now(
        &self,
        time: Duration,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>> {
        (**self).sleep_now(time)
    }

    fn task_wasm(&self, task: TaskWasm) -> Result<(), WasiThreadError> {
        (**self).task_wasm(task)
    }

    fn thread_parallelism(&self) -> Result<usize, WasiThreadError> {
        (**self).thread_parallelism()
    }
}
