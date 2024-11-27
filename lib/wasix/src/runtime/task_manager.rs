use std::ops::Deref;
use std::{pin::Pin, time::Duration};

use derivative::Derivative;
use futures::future::LocalBoxFuture;
use futures::Future;
use wasm_bindgen::JsValue;
use wasmer::{Memory, MemoryType, Module, Store, StoreMut, StoreRef};

use crate::os::task::thread::WasiThreadError;
use crate::{StoreSnapshot, WasiEnv, WasiFunctionEnv};

pub use virtual_mio::waker::*;

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
pub type TaskWasmRun =
    dyn FnOnce(TaskWasmRunProperties) -> LocalBoxFuture<'static, ()> + Send + 'static;

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
#[allow(unused_variables)]
pub trait VirtualTaskManager: std::fmt::Debug + Send + Sync + 'static {
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

    /// Run an asynchronous operation on the thread pool.
    ///
    /// This task must not block execution or it could cause deadlocks.
    ///
    /// See the "Thread Safety" documentation on [`VirtualTaskManager`] for
    /// limitations on what a `task` can and can't contain.
    fn task_shared(
        &self,
        task: Box<dyn FnOnce() -> LocalBoxFuture<'static, ()> + Send + 'static>,
    ) -> Result<(), WasiThreadError>;

    /// Run a WebAssembly operation on the thread pool.
    ///
    /// This is primarily used inside the context of a syscall and allows
    /// the transfer of things like [`wasmer::Module`] across threads.
    fn task_wasm(&self, task: TaskWasm) -> Result<(), WasiThreadError>;

    /// Returns the amount of parallelism that is possible on this platform.
    fn thread_parallelism(&self) -> Result<usize, WasiThreadError>;

    /// Schedule a task to run on the threadpool, explicitly
    /// transferring a [`Module`] to the task.
    ///
    /// This should be preferred over [`VirtualTaskManager::task_dedicated()`]
    /// where possible because [`wasmer::Module`] is actually `!Send` in the
    /// browser and can only be transferred to background threads via
    /// an explicit `postMessage()`. See [#4158] for more details.
    ///
    /// This is very similar to [`VirtualTaskManager::task_wasm()`], but
    /// intended for use outside of a syscall context. For example, when you are
    /// running in the browser and want to run a WebAssembly module in the
    /// background.
    ///
    /// [#4158]: https://github.com/wasmerio/wasmer/issues/4158
    fn spawn_with_module(
        &self,
        module: Module,
        task: Box<dyn FnOnce(Module) -> LocalBoxFuture<'static, ()> + Send + 'static>,
    ) -> Result<(), WasiThreadError> {
        // Note: Ideally, this function and task_wasm() would be superseded by
        // a more general mechanism for transferring non-thread safe values
        // to the thread pool.
        self.task_shared(Box::new(move || task(module)))
    }
}

impl<D, T> VirtualTaskManager for D
where
    D: Deref<Target = T> + std::fmt::Debug + Send + Sync + 'static,
    T: VirtualTaskManager + ?Sized,
{
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

    fn task_shared(
        &self,
        task: Box<dyn FnOnce() -> LocalBoxFuture<'static, ()> + Send + 'static>,
    ) -> Result<(), WasiThreadError> {
        (**self).task_shared(task)
    }

    fn task_wasm(&self, task: TaskWasm) -> Result<(), WasiThreadError> {
        (**self).task_wasm(task)
    }

    fn thread_parallelism(&self) -> Result<usize, WasiThreadError> {
        (**self).thread_parallelism()
    }

    fn spawn_with_module(
        &self,
        module: Module,
        task: Box<dyn FnOnce(Module) -> LocalBoxFuture<'static, ()> + Send + 'static>,
    ) -> Result<(), WasiThreadError> {
        (**self).spawn_with_module(module, task)
    }
}

/// Generic utility methods for VirtualTaskManager
pub trait VirtualTaskManagerExt {
    /// Runs the work in the background via the task managers shared background
    /// threads while blocking the current execution until it finishs
    fn spawn_and_block_on<A>(
        &self,
        task: impl Future<Output = A> + Send + 'static,
    ) -> Result<A, anyhow::Error>
    where
        A: Send + 'static;
}

impl<D, T> VirtualTaskManagerExt for D
where
    D: Deref<Target = T>,
    T: VirtualTaskManager + ?Sized,
{
    /// Runs the work in the background via the task managers shared background
    /// threads while blocking the current execution until it finishs
    fn spawn_and_block_on<A>(
        &self,
        task: impl Future<Output = A> + Send + 'static,
    ) -> Result<A, anyhow::Error>
    where
        A: Send + 'static,
    {
        let (tx, rx) = ::tokio::sync::oneshot::channel();
        let work = Box::pin(async move {
            let ret = task.await;
            tx.send(ret).ok();
        });
        self.task_shared(Box::new(move || work)).unwrap();
        rx.blocking_recv()
            .map_err(|_| anyhow::anyhow!("task execution failed - result channel dropped"))
    }
}
