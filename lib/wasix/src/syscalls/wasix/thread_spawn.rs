use super::*;
use crate::{
    capture_store_snapshot,
    runtime::{
        task_manager::{TaskWasm, TaskWasmRunProperties},
        TaintReason,
    },
    syscalls::*,
    WasiThreadHandle,
};

use future::FutureExt;
use tokio::sync::oneshot;
use wasmer_wasix_types::wasi::{ThreadActions, ThreadStart};

/// ### `thread_spawn()`
/// Creates a new thread by spawning that shares the same
/// memory address space, file handles and main event loops.
///
/// ## Parameters
///
/// * `start_ptr` - Pointer to the structure that describes the thread to be launched
/// * `ret_tid` - ID of the thread that was launched
///
/// ## Return
///
/// Returns the thread index of the newly created thread
/// (indices always start from the same value as `pid` and increments in steps)
#[instrument(level = "trace", skip_all, ret)]
pub fn thread_spawn_v2<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    start_ptr: WasmPtr<ThreadStart<M>, M>,
    ret_tid: WasmPtr<Tid, M>,
) -> Errno {
    // Create the thread
    let tid = wasi_try!(thread_spawn_internal_from_wasi(&mut ctx, start_ptr));

    // Success
    let memory = unsafe { ctx.data().memory_view(&ctx) };
    wasi_try_mem!(ret_tid.write(&memory, tid));

    tracing::debug!(
        tid,
        from_tid = ctx.data().thread.id().raw(),
        "spawned new thread"
    );

    Errno::Success
}

pub fn thread_spawn_internal_from_wasi<M: MemorySize>(
    ctx: &mut FunctionEnvMut<'_, WasiEnv>,
    start_ptr: WasmPtr<ThreadStart<M>, M>,
) -> Result<Tid, Errno> {
    // Now we use the environment and memory references
    let env = ctx.data();
    let start_ptr_offset = start_ptr.offset();

    // Create the handle that represents this thread
    let thread_start = ThreadStartType::ThreadSpawn {
        start_ptr: start_ptr_offset.into(),
    };
    let thread_handle = match env.process.new_thread(thread_start) {
        Ok(h) => Arc::new(h),
        Err(err) => {
            error!(
                err = %err,
                "failed to create thread handle",
            );
            // TODO: evaluate the appropriate error code, document it in the spec.
            return Err(Errno::Access);
        }
    };
    let thread_id: Tid = thread_handle.id().into();
    Span::current().record("tid", thread_id);

    // Spawn the thread
    thread_spawn_internal_using_layout::<M>(ctx, thread_handle, start_ptr_offset)?;

    // Success
    Ok(thread_id)
}

pub fn thread_spawn_internal_using_layout<M: MemorySize>(
    ctx: &mut FunctionEnvMut<'_, WasiEnv>,
    thread_handle: Arc<WasiThreadHandle>,
    start_ptr_offset: M::Offset,
) -> Result<(), Errno> {
    // We extract the memory which will be passed to the thread
    let env = ctx.data();
    let tasks = env.tasks().clone();
    let thread_memory = unsafe { env.inner() }.memory_clone();

    // We capture some local variables
    let mut thread_env = env.clone();
    thread_env.thread = thread_handle.as_thread();

    // This next function gets a context for the local thread and then
    // calls into the process
    let execute_module = {
        let thread_handle = thread_handle;
        move |ctx: WasiFunctionEnv, store: Store| {
            // Call the thread
            call_module::<M>(ctx, store, start_ptr_offset, thread_handle)
        }
    };

    // If the process does not export a thread spawn function then obviously
    // we can't spawn a background thread
    if unsafe { env.inner() }.thread_start.is_none() {
        warn!("thread failed - the program does not export a `wasi_thread_start` function");
        return Err(Errno::Notcapable);
    }
    let thread_module = unsafe { env.inner() }.module().clone();
    let globals = capture_store_snapshot(&mut ctx.as_store_mut());
    let spawn_type =
        crate::runtime::SpawnMemoryType::ShareMemory(thread_memory, ctx.as_store_ref());

    // Now spawn a thread
    trace!("threading: spawning background thread");
    let run = move |props: TaskWasmRunProperties| {
        async move {
            if let Err(err) = execute_module(props.ctx, props.store).await {
                tracing::warn!("Starting thread failed: {err}");
            }
        }
        .boxed_local()
    };
    tasks
        .task_wasm(
            TaskWasm::new(Box::new(run), thread_env, thread_module)
                .with_globals(&globals)
                .with_memory(spawn_type),
        )
        .map_err(Into::<Errno>::into)?;

    // Success
    Ok(())
}

/// Calls the module
async fn call_module<M: MemorySize>(
    env: WasiFunctionEnv,
    mut store: Store,
    start_ptr_offset: M::Offset,
    thread_handle: Arc<WasiThreadHandle>,
) -> Result<Tid, Errno> {
    let tid = env.data(&store).tid();

    // Call thread-start callback.
    let thread_start = unsafe { env.data(&store).inner() }
        .thread_start
        .clone()
        .unwrap();
    let mut call_ret = thread_start.call(
        &mut store,
        tid.raw().try_into().map_err(|_| Errno::Overflow).unwrap(),
        start_ptr_offset
            .try_into()
            .map_err(|_| Errno::Overflow)
            .unwrap(),
    );

    // Check for thread holding.
    let wasi_env = env.data_mut(&mut store);
    if let Some(release_rx) = wasi_env.thread_release_rx.take() {
        wasi_env.thread_start_executed = true;

        // Wait for thread release.
        // This "await" releases control back to the brower event loop and allows
        // callbacks and promises to execute.
        tracing::debug!("thread is on hold");
        let _ = release_rx.await;

        tracing::debug!("thread hold released, cleaning up");
        if let Some(thread_set_actions) = unsafe { env.data(&store).inner() }
            .thread_set_actions
            .clone()
        {
            if let Err(err) = thread_set_actions.call(&mut store, ThreadActions::NO_START.bits()) {
                warn!("cannot set thread actions for cleanup: {err}");
            }
        }

        call_ret = thread_start.call(
            &mut store,
            tid.raw().try_into().map_err(|_| Errno::Overflow).unwrap(),
            start_ptr_offset
                .try_into()
                .map_err(|_| Errno::Overflow)
                .unwrap(),
        );
        tracing::debug!("thread cleanup finished");
    }

    let mut ret = Errno::Success;
    if let Err(err) = call_ret {
        match err.downcast::<WasiError>() {
            Ok(WasiError::Exit(code)) => {
                ret = if code.is_success() {
                    Errno::Success
                } else {
                    env.data(&store)
                        .runtime
                        .on_taint(TaintReason::NonZeroExitCode(code));
                    Errno::Noexec
                };
            }
            Ok(WasiError::UnknownWasiVersion) => {
                debug!("failed as wasi version is unknown",);
                env.data(&store)
                    .runtime
                    .on_taint(TaintReason::UnknownWasiVersion);
                ret = Errno::Noexec;
            }
            Err(err) => {
                debug!("failed with runtime error: {}", err);
                env.data(&store)
                    .runtime
                    .on_taint(TaintReason::RuntimeError(err));
                ret = Errno::Noexec;
            }
        }
    }
    trace!("callback finished (ret={})", ret);

    // Clean up the environment
    env.on_exit(&mut store, Some(ret.into()));

    // Frees the handle so that it closes
    drop(thread_handle);
    Ok(ret as Pid)
}

pub(crate) fn thread_hold_internal<M: MemorySize + 'static>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
) -> Result<Errno, WasiError> {
    let env = ctx.data_mut();
    let tid = env.tid();

    let Some(thread_set_actions) = unsafe { env.inner() }.thread_set_actions.clone() else {
        tracing::error!(tid = %tid, "attempt to hold thread without exporting wasi_thread_set_actions");
        return Ok(Errno::Notsup);
    };

    if env.thread_start_executed {
        tracing::error!(tid = %tid, "attempt to hold thread in cleanup");
        return Ok(Errno::Already);
    }

    let (tx, rx) = oneshot::channel();
    env.thread_release_tx = Some(tx);
    env.thread_release_rx = Some(rx);

    if let Err(err) = thread_set_actions.call(&mut ctx, ThreadActions::NO_FINISH.bits()) {
        tracing::error!("cannot set thread actions for holding: {err}");
    }

    tracing::debug!(tid = %tid, "thread will be held after start function finishes");
    Ok(Errno::Success)
}

pub(crate) fn thread_release_internal<M: MemorySize + 'static>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
) -> Result<Errno, WasiError> {
    let env = ctx.data_mut();
    let tid = env.tid();

    let Some(tx) = env.thread_release_tx.take() else {
        tracing::error!(tid = %tid, "attempt to release thread that is not being held");
        return Ok(Errno::Already);
    };

    let _ = tx.send(());

    tracing::debug!(tid = %tid, "held thread has been released");
    Ok(Errno::Success)
}
