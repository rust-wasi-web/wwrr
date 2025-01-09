use std::{ops::Deref, sync::Arc, time::Duration};

use derivative::Derivative;
use futures::future::BoxFuture;
use tokio::sync::oneshot;
use virtual_fs::{FsError, VirtualFile};
use virtual_net::DynVirtualNetworking;
use wasmer::{
    AsStoreMut, AsStoreRef, FunctionEnvMut, Imports, ImportsObj, Instance, Memory, MemoryType,
    MemoryView, Module, TypedFunction, Value,
};
use wasmer_wasix_types::{
    types::Signal,
    wasi::{Errno, ExitCode, Snapshot0Clockid},
    wasix::ThreadStartType,
};

use crate::{
    fs::{WasiFsRoot, WasiInodes},
    import_object_for_all_wasi_versions,
    os::task::{
        process::{WasiProcess, WasiProcessId},
        thread::{WasiThread, WasiThreadHandle, WasiThreadId},
    },
    runtime::{task_manager::InlineWaker, SpawnMemoryType},
    syscalls::platform_clock_time_get,
    Runtime, VirtualTaskManager, WasiControlPlane, WasiEnvBuilder, WasiError, WasiFunctionEnv,
    WasiResult, WasiRuntimeError,
};

pub(crate) use super::handles::*;
use super::WasiState;

/// Various [`TypedFunction`] and [`Global`] handles for an active WASI(X) instance.
///
/// Used to access and modify runtime state.
// TODO: make fields private
#[derive(Derivative, Clone)]
#[derivative(Debug)]
pub struct WasiInstanceHandles {
    /// Represents a reference to the memory
    pub(crate) memory: Memory,
    /// WebAssembly instance.
    pub(crate) instance: wasmer::Instance,

    /// Represents the callback for starting a thread (name = "wasi_thread_start")
    /// (due to limitations with i64 in browsers the parameters are broken into i32 pairs)
    /// [this takes a user_data field]
    #[derivative(Debug = "ignore")]
    pub(crate) thread_start: Option<TypedFunction<(i32, i32), ()>>,

    /// Represents the callback for setting the thread actions (name = "wasi_thread_set_actions")
    #[derivative(Debug = "ignore")]
    pub(crate) thread_set_actions: Option<TypedFunction<i32, ()>>,

    /// Represents the callback for signals (name = "__wasm_signal")
    /// Signals are triggered asynchronously at idle times of the process
    #[derivative(Debug = "ignore")]
    pub(crate) signal: Option<TypedFunction<i32, ()>>,

    /// Flag that indicates if the signal callback has been set by the WASM
    /// process - if it has not been set then the runtime behaves differently
    /// when a CTRL-C is pressed.
    pub(crate) signal_set: bool,
}

impl WasiInstanceHandles {
    pub fn new(memory: Memory, store: &impl AsStoreRef, instance: Instance) -> Self {
        WasiInstanceHandles {
            memory,
            thread_start: instance
                .exports
                .get_typed_function(store, "wasi_thread_start")
                .ok(),
            thread_set_actions: instance
                .exports
                .get_typed_function(store, "wasi_thread_set_actions")
                .ok(),
            signal: instance
                .exports
                .get_typed_function(&store, "__wasm_signal")
                .ok(),
            signal_set: false,
            instance,
        }
    }

    /// WebAssembly module.
    pub fn module(&self) -> &Module {
        self.instance.module()
    }

    /// Providers safe access to the memory
    /// (it must be initialized before it can be used)
    pub fn memory_view<'a>(&'a self, store: &'a (impl AsStoreRef + ?Sized)) -> MemoryView<'a> {
        self.memory.view(store)
    }

    /// Providers safe access to the memory
    /// (it must be initialized before it can be used)
    pub fn memory(&self) -> &Memory {
        &self.memory
    }

    /// Copy the lazy reference so that when it's initialized during the
    /// export phase, all the other references get a copy of it
    pub fn memory_clone(&self) -> Memory {
        self.memory.clone()
    }

    /// WebAssembly instance.
    pub fn instance(&self) -> &Instance {
        &self.instance
    }
}

/// Data required to construct a [`WasiEnv`].
#[derive(Debug)]
pub struct WasiEnvInit {
    pub(crate) state: WasiState,
    pub runtime: Arc<dyn Runtime + Send + Sync>,

    pub control_plane: WasiControlPlane,
    pub memory_ty: Option<MemoryType>,
    pub process: Option<WasiProcess>,
    pub thread: Option<WasiThreadHandle>,

    /// Whether to call the `_initialize` function in the WASI module.
    /// Will be true for regular new instances, but false for threads.
    pub call_initialize: bool,

    /// Additional functionality provided to the WASIX instance, besides the
    /// normal WASIX syscalls.
    pub additional_imports: Imports,

    /// Name of wasm-bindgen generated JavaScript module.
    pub wbg_js_module_name: String,
}

impl WasiEnvInit {
    pub fn duplicate(&self) -> Self {
        let inodes = WasiInodes::new();

        // TODO: preserve preopens?
        let fs =
            crate::fs::WasiFs::new_with_preopen(&inodes, &[], &[], self.state.fs.root_fs.clone())
                .unwrap();

        Self {
            state: WasiState {
                inodes,
                fs,
                futexs: Default::default(),
                clock_offset: std::sync::Mutex::new(
                    self.state.clock_offset.lock().unwrap().clone(),
                ),
                args: self.state.args.clone(),
                envs: std::sync::Mutex::new(self.state.envs.lock().unwrap().deref().clone()),
                preopen: self.state.preopen.clone(),
            },
            runtime: self.runtime.clone(),
            control_plane: self.control_plane.clone(),
            memory_ty: None,
            process: None,
            thread: None,
            call_initialize: self.call_initialize,
            additional_imports: self.additional_imports.clone(),
            wbg_js_module_name: self.wbg_js_module_name.clone(),
        }
    }
}

/// The environment provided to the WASI imports.
pub struct WasiEnv {
    pub control_plane: WasiControlPlane,
    /// Represents the process this environment is attached to
    pub process: WasiProcess,
    /// Represents the thread this environment is attached to
    pub thread: WasiThread,
    /// Seed used to rotate around the events returned by `poll_oneoff`
    pub poll_seed: u64,
    /// Shared state of the WASI system. Manages all the data that the
    /// executing WASI program can see.
    pub(crate) state: Arc<WasiState>,
    /// List of the handles that are owned by this context
    /// (this can be used to ensure that threads own themselves or others)
    pub owned_handles: Vec<WasiThreadHandle>,
    /// Implementation of the WASI runtime.
    pub runtime: Arc<dyn Runtime + Send + Sync + 'static>,

    /// Flag that indicates the cleanup of the environment is to be disabled
    /// (this is normally used so that the instance can be reused later on)
    pub(crate) disable_fs_cleanup: bool,

    /// Whether the thread start was executed.
    pub(crate) thread_start_executed: bool,
    /// Triggers the finishing of a held thread.
    pub(crate) thread_release_tx: Option<oneshot::Sender<()>>,
    /// Receives the trigger for finishing a held thread.
    pub(crate) thread_release_rx: Option<oneshot::Receiver<()>>,

    /// Inner functions and references that are loaded before the environment starts
    /// (inner is not safe to send between threads and so it is private and will
    ///  not be cloned when `WasiEnv` is cloned)
    /// TODO: We should move this outside of `WasiEnv` with some refactoring
    inner: WasiInstanceHandlesPointer,
}

impl std::fmt::Debug for WasiEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "env(pid={}, tid={})", self.pid().raw(), self.tid().raw())
    }
}

impl Clone for WasiEnv {
    fn clone(&self) -> Self {
        Self {
            control_plane: self.control_plane.clone(),
            process: self.process.clone(),
            poll_seed: self.poll_seed,
            thread: self.thread.clone(),
            state: self.state.clone(),
            inner: Default::default(),
            owned_handles: self.owned_handles.clone(),
            runtime: self.runtime.clone(),
            disable_fs_cleanup: self.disable_fs_cleanup,
            thread_start_executed: Default::default(),
            thread_release_tx: Default::default(),
            thread_release_rx: Default::default(),
        }
    }
}

impl WasiEnv {
    /// Construct a new [`WasiEnvBuilder`] that allows customizing an environment.
    pub fn builder(program_name: impl Into<String>) -> WasiEnvBuilder {
        WasiEnvBuilder::new(program_name)
    }

    pub fn pid(&self) -> WasiProcessId {
        self.process.pid()
    }

    pub fn tid(&self) -> WasiThreadId {
        self.thread.tid()
    }

    #[allow(clippy::result_large_err)]
    pub(crate) fn from_init(init: WasiEnvInit) -> Result<Self, WasiRuntimeError> {
        let process = if let Some(p) = init.process {
            p
        } else {
            init.control_plane.new_process()?
        };

        let thread = if let Some(t) = init.thread {
            t
        } else {
            process.new_thread(ThreadStartType::MainThread)?
        };

        let mut env = Self {
            control_plane: init.control_plane,
            process,
            thread: thread.as_thread(),
            poll_seed: 0,
            state: Arc::new(init.state),
            inner: Default::default(),
            owned_handles: Vec::new(),
            runtime: init.runtime,
            disable_fs_cleanup: false,
            thread_start_executed: false,
            thread_release_tx: None,
            thread_release_rx: None,
        };
        env.owned_handles.push(thread);

        Ok(env)
    }

    // FIXME: use custom error type
    #[allow(clippy::result_large_err)]
    pub(crate) async fn instantiate(
        mut init: WasiEnvInit,
        module: Module,
        store: &mut impl AsStoreMut,
        imports_obj: ImportsObj,
    ) -> Result<(Instance, WasiFunctionEnv), WasiRuntimeError> {
        let call_initialize = init.call_initialize;
        let spawn_type = init.memory_ty.take();

        for import in module.imports() {
            tracing::trace!("import {}.{}", import.module(), import.name());
        }

        let additional_imports = init.additional_imports.clone();
        let wbg_js_module_name = init.wbg_js_module_name.clone();

        let env = Self::from_init(init)?;
        let pid = env.process.pid();

        let mut store = store.as_store_mut();

        let tasks = env.runtime.task_manager().clone();
        let mut func_env = WasiFunctionEnv::new(&mut store, env);

        // Determine if shared memory needs to be created and imported
        let shared_memory = module.imports().memories().next().map(|a| *a.ty());

        // Determine if we are going to create memory and import it or just rely on self creation of memory
        let spawn_type = if let Some(t) = spawn_type {
            SpawnMemoryType::CreateMemoryOfType(t)
        } else {
            match shared_memory {
                Some(ty) => SpawnMemoryType::CreateMemoryOfType(ty),
                None => SpawnMemoryType::CreateMemory,
            }
        };
        let memory = tasks.build_memory(&mut store, spawn_type)?;

        // Let's instantiate the module with the imports.
        let (mut imports, instance_init_callback) =
            import_object_for_all_wasi_versions(&module, &mut store, &func_env.env);

        for ((namespace, name), value) in &additional_imports {
            // Note: We don't want to let downstream users override WASIX
            // syscalls
            if !imports.exists(&namespace, &name) {
                imports.define(&namespace, &name, value);
            }
        }

        let imported_memory = if let Some(memory) = memory {
            imports.define("env", "memory", memory.clone());
            Some(memory)
        } else {
            None
        };

        // Construct the instance.
        let instance = match Instance::new(&mut store, &module, &imports, imports_obj).await {
            Ok(a) => a,
            Err(err) => {
                tracing::error!(
                    %pid,
                    error = &err as &dyn std::error::Error,
                    "Instantiation failed",
                );
                func_env
                    .data(&store)
                    .blocking_on_exit(Some(Errno::Noexec.into()));
                return Err(err.into());
            }
        };

        // Run initializers.
        instance_init_callback(&instance, &store).unwrap();

        // Initialize the WASI environment
        if let Err(err) =
            func_env.initialize_with_memory(&mut store, instance.clone(), imported_memory)
        {
            tracing::error!(
                %pid,
                error = &err as &dyn std::error::Error,
                "Initialization failed",
            );
            func_env
                .data(&store)
                .blocking_on_exit(Some(Errno::Noexec.into()));
            return Err(err.into());
        }

        // Set number of processors.
        if let Ok(nprocessors) = instance.exports.get_global("wasi_nprocessors").cloned() {
            match tasks.thread_parallelism() {
                Ok(p) => {
                    if let Err(err) = nprocessors.set(&mut store, Value::I32(p as _)) {
                        tracing::warn!("cannot set number of processors: {err}");
                    }
                }
                Err(err) => tracing::warn!("cannot get number of processors: {err}"),
            }
        }

        // Initialize the task manager.
        let memory = func_env.data(&store).try_memory_clone().unwrap();
        func_env
            .data(&store)
            .tasks()
            .init(module, memory, wbg_js_module_name)
            .await;

        // If this module exports an _initialize function, run that first.
        if call_initialize {
            if let Ok(initialize) = instance.exports.get_function("_initialize") {
                tracing::info!("calling WASI _initialize");
                if let Err(err) = crate::run_wasi_func_start(initialize, &mut store) {
                    func_env
                        .data(&store)
                        .blocking_on_exit(Some(Errno::Noexec.into()));
                    return Err(err);
                }
            }
        }

        Ok((instance, func_env))
    }

    /// Returns a copy of the current runtime implementation for this environment
    pub fn runtime(&self) -> &(dyn Runtime + Send + Sync) {
        self.runtime.deref()
    }

    /// Returns a copy of the current tasks implementation for this environment
    pub fn tasks(&self) -> &Arc<dyn VirtualTaskManager> {
        self.runtime.task_manager()
    }

    pub fn fs_root(&self) -> &WasiFsRoot {
        &self.state.fs.root_fs
    }

    /// Overrides the runtime implementation for this environment
    pub fn set_runtime<R>(&mut self, runtime: R)
    where
        R: Runtime + Send + Sync + 'static,
    {
        self.runtime = Arc::new(runtime);
    }

    /// Returns the number of active threads
    pub fn active_threads(&self) -> u32 {
        self.process.active_threads()
    }

    /// Porcesses any signals that are batched up or any forced exit codes
    pub fn process_signals_and_exit(ctx: &mut FunctionEnvMut<'_, Self>) -> WasiResult<bool> {
        // If a signal handler has never been set then we need to handle signals
        // differently
        let env = ctx.data();
        let inner = env
            .try_inner()
            .ok_or_else(|| WasiError::Exit(Errno::Fault.into()))?;
        if !inner.signal_set {
            let signals = env.thread.pop_signals();
            if !signals.is_empty() {
                for sig in signals {
                    if sig == Signal::Sigint
                        || sig == Signal::Sigquit
                        || sig == Signal::Sigkill
                        || sig == Signal::Sigabrt
                    {
                        let exit_code = env.thread.set_or_get_exit_code_for_signal(sig);
                        return Err(WasiError::Exit(exit_code));
                    } else {
                        tracing::trace!(pid=%env.pid(), ?sig, "Signal ignored");
                    }
                }
                return Ok(Ok(true));
            }
        }

        // Check for forced exit
        if let Some(forced_exit) = env.should_exit() {
            return Err(WasiError::Exit(forced_exit));
        }

        Self::process_signals(ctx)
    }

    /// Porcesses any signals that are batched up
    pub(crate) fn process_signals(ctx: &mut FunctionEnvMut<'_, Self>) -> WasiResult<bool> {
        // If a signal handler has never been set then we need to handle signals
        // differently
        let env = ctx.data();
        let inner = env
            .try_inner()
            .ok_or_else(|| WasiError::Exit(Errno::Fault.into()))?;
        if !inner.signal_set {
            return Ok(Ok(false));
        }

        // Check for any signals that we need to trigger
        // (but only if a signal handler is registered)
        let ret = if inner.signal.as_ref().is_some() {
            let signals = env.thread.pop_signals();
            Self::process_signals_internal(ctx, signals)?
        } else {
            false
        };

        Ok(Ok(ret))
    }

    pub(crate) fn process_signals_internal(
        ctx: &mut FunctionEnvMut<'_, Self>,
        mut signals: Vec<Signal>,
    ) -> Result<bool, WasiError> {
        let env = ctx.data();
        let inner = env
            .try_inner()
            .ok_or_else(|| WasiError::Exit(Errno::Fault.into()))?;
        if let Some(handler) = inner.signal.clone() {
            // We might also have signals that trigger on timers
            let mut now = 0;
            {
                let mut has_signal_interval = false;
                let inner = env.process.inner.0.lock().unwrap();
                if !inner.signal_intervals.is_empty() {
                    now = platform_clock_time_get(Snapshot0Clockid::Monotonic, 1_000_000).unwrap()
                        as u128;
                    for signal in inner.signal_intervals.values() {
                        let elapsed = now - signal.last_signal;
                        if elapsed >= signal.interval.as_nanos() {
                            has_signal_interval = true;
                            break;
                        }
                    }
                }
                if has_signal_interval {
                    let mut inner = env.process.inner.0.lock().unwrap();
                    for signal in inner.signal_intervals.values_mut() {
                        let elapsed = now - signal.last_signal;
                        if elapsed >= signal.interval.as_nanos() {
                            signal.last_signal = now;
                            signals.push(signal.signal);
                        }
                    }
                }
            }

            for signal in signals {
                tracing::trace!(
                    pid=%ctx.data().pid(),
                    ?signal,
                    "processing signal via handler",
                );
                if let Err(err) = handler.call(ctx, signal as i32) {
                    match err.downcast::<WasiError>() {
                        Ok(wasi_err) => {
                            tracing::warn!(
                                pid=%ctx.data().pid(),
                                wasi_err=&wasi_err as &dyn std::error::Error,
                                "signal handler wasi error",
                            );
                            return Err(wasi_err);
                        }
                        Err(runtime_err) => {
                            // anything other than a kill command should report
                            // the error, killed things may not gracefully close properly
                            if signal != Signal::Sigkill {
                                tracing::warn!(
                                    pid=%ctx.data().pid(),
                                    runtime_err=&runtime_err as &dyn std::error::Error,
                                    "signal handler runtime error",
                                );
                            }
                            return Err(WasiError::Exit(Errno::Intr.into()));
                        }
                    }
                }
                tracing::trace!(
                    pid=%ctx.data().pid(),
                    "signal processed",
                );
            }
            Ok(true)
        } else {
            tracing::trace!("no signal handler");
            Ok(false)
        }
    }

    /// Returns an exit code if the thread or process has been forced to exit
    pub fn should_exit(&self) -> Option<ExitCode> {
        // Check for forced exit
        if let Some(forced_exit) = self.thread.try_join() {
            return Some(forced_exit.unwrap_or_else(|err| {
                tracing::debug!(
                    error = &*err as &dyn std::error::Error,
                    "exit runtime error",
                );
                Errno::Child.into()
            }));
        }
        if let Some(forced_exit) = self.process.try_join() {
            return Some(forced_exit.unwrap_or_else(|err| {
                tracing::debug!(
                    error = &*err as &dyn std::error::Error,
                    "exit runtime error",
                );
                Errno::Child.into()
            }));
        }
        None
    }

    /// Accesses the virtual networking implementation
    pub fn net(&self) -> &DynVirtualNetworking {
        self.runtime.networking()
    }

    /// Providers safe access to the initialized part of WasiEnv
    /// (it must be initialized before it can be used)
    /// This has been marked as unsafe as it will panic if its executed
    /// on the wrong thread or before the inner is set
    pub(crate) unsafe fn inner(&self) -> WasiInstanceGuard<'_> {
        self.inner.get().expect(
            "You must initialize the WasiEnv before using it and can not pass it between threads",
        )
    }

    /// Providers safe access to the initialized part of WasiEnv
    pub(crate) fn try_inner(&self) -> Option<WasiInstanceGuard<'_>> {
        self.inner.get()
    }

    /// Providers safe access to the initialized part of WasiEnv
    /// (it must be initialized before it can be used)
    pub(crate) fn try_inner_mut(&mut self) -> Option<WasiInstanceGuardMut<'_>> {
        self.inner.get_mut()
    }

    /// Sets the inner object (this should only be called when
    /// creating the instance and eventually should be moved out
    /// of the WasiEnv)
    #[doc(hidden)]
    pub(crate) fn set_inner(&mut self, handles: WasiInstanceHandles) {
        self.inner.set(handles)
    }

    /// Tries to clone the instance from this environment
    pub fn try_clone_instance(&self) -> Option<Instance> {
        self.inner.get().map(|i| i.instance.clone())
    }

    /// Providers safe access to the memory
    /// (it must be initialized before it can be used)
    pub(crate) fn try_memory(&self) -> Option<WasiInstanceGuardMemory<'_>> {
        self.try_inner().map(|i| i.memory())
    }

    /// Providers safe access to the memory
    /// (it must be initialized before it can be used)
    pub(crate) fn try_memory_view<'a>(
        &self,
        store: &'a (impl AsStoreRef + ?Sized),
    ) -> Option<MemoryView<'a>> {
        self.try_memory().map(|m| m.view(store))
    }

    /// Providers safe access to the memory
    /// (it must be initialized before it can be used)
    /// This has been marked as unsafe as it will panic if its executed
    /// on the wrong thread or before the inner is set
    pub(crate) unsafe fn memory_view<'a>(
        &self,
        store: &'a (impl AsStoreRef + ?Sized),
    ) -> MemoryView<'a> {
        self.try_memory_view(store).expect(
            "You must initialize the WasiEnv before using it and can not pass it between threads",
        )
    }

    /// Copy the lazy reference so that when it's initialized during the
    /// export phase, all the other references get a copy of it
    #[allow(dead_code)]
    pub(crate) fn try_memory_clone(&self) -> Option<Memory> {
        self.try_inner().map(|i| i.memory_clone())
    }

    /// Get the WASI state
    pub(crate) fn state(&self) -> &WasiState {
        &self.state
    }

    /// Get the `VirtualFile` object at stdout
    pub fn stdout(&self) -> Result<Option<Box<dyn VirtualFile + Send + Sync + 'static>>, FsError> {
        self.state.stdout()
    }

    /// Get the `VirtualFile` object at stderr
    pub fn stderr(&self) -> Result<Option<Box<dyn VirtualFile + Send + Sync + 'static>>, FsError> {
        self.state.stderr()
    }

    /// Get the `VirtualFile` object at stdin
    pub fn stdin(&self) -> Result<Option<Box<dyn VirtualFile + Send + Sync + 'static>>, FsError> {
        self.state.stdin()
    }

    /// Internal helper function to get a standard device handle.
    /// Expects one of `__WASI_STDIN_FILENO`, `__WASI_STDOUT_FILENO`, `__WASI_STDERR_FILENO`.
    pub fn std_dev_get(
        &self,
        fd: crate::syscalls::WasiFd,
    ) -> Result<Option<Box<dyn VirtualFile + Send + Sync + 'static>>, FsError> {
        self.state.std_dev_get(fd)
    }

    /// Unsafe:
    ///
    /// This will access the memory of the WASM process and create a view into it which is
    /// inherently unsafe as it could corrupt the memory. Also accessing the memory is not
    /// thread safe.
    pub(crate) unsafe fn get_memory_and_wasi_state<'a>(
        &'a self,
        store: &'a impl AsStoreRef,
        _mem_index: u32,
    ) -> (MemoryView<'a>, &'a WasiState) {
        let memory = self.memory_view(store);
        let state = self.state.deref();
        (memory, state)
    }

    /// Unsafe:
    ///
    /// This will access the memory of the WASM process and create a view into it which is
    /// inherently unsafe as it could corrupt the memory. Also accessing the memory is not
    /// thread safe.
    pub(crate) unsafe fn get_memory_and_wasi_state_and_inodes<'a>(
        &'a self,
        store: &'a impl AsStoreRef,
        _mem_index: u32,
    ) -> (MemoryView<'a>, &'a WasiState, &'a WasiInodes) {
        let memory = self.memory_view(store);
        let state = self.state.deref();
        let inodes = &state.inodes;
        (memory, state, inodes)
    }

    /// Cleans up all the open files (if this is the main thread)
    #[allow(clippy::await_holding_lock)]
    pub fn blocking_on_exit(&self, exit_code: Option<ExitCode>) {
        let cleanup = self.on_exit(exit_code);
        InlineWaker::block_on(cleanup);
    }

    /// Cleans up all the open files (if this is the main thread)
    #[allow(clippy::await_holding_lock)]
    pub fn on_exit(&self, exit_code: Option<ExitCode>) -> BoxFuture<'static, ()> {
        const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

        // If this is the main thread then also close all the files
        if self.thread.is_main() {
            let process = self.process.clone();
            let disable_fs_cleanup = self.disable_fs_cleanup;
            let pid = self.pid();

            let timeout = self.tasks().sleep_now(CLEANUP_TIMEOUT);
            let state = self.state.clone();
            Box::pin(async move {
                if !disable_fs_cleanup {
                    tracing::trace!(pid = %pid, "cleaning up open file handles");

                    // Perform the clean operation using the asynchronous runtime
                    tokio::select! {
                        _ = timeout => {
                            tracing::debug!(
                                "WasiEnv::cleanup has timed out after {CLEANUP_TIMEOUT:?}"
                            );
                        },
                        _ = state.fs.close_all() => { }
                    }

                    // Now send a signal that the thread is terminated
                    process.signal_process(Signal::Sigquit);
                }

                // Terminate the process
                let exit_code = exit_code.unwrap_or_else(|| Errno::Canceled.into());
                process.terminate(exit_code);
            })
        } else {
            Box::pin(async {})
        }
    }
}
