use super::*;
use crate::syscalls::*;

use wasmer_wasix_types::wasi::ThreadStart;

/// ### `thread_spawn()`
/// Creates a new thread by spawning that shares the same
/// memory address space, file handles and main event loops.
///
/// ## Parameters
///
/// * `start_ptr` - Pointer to the structure that describes the thread to be launched
///
/// ## Return
///
/// Returns the thread index of the newly created thread
/// (indices always start from the same value as `pid` and increments in steps)
#[instrument(level = "trace", skip_all, ret)]
pub fn thread_spawn<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    start_ptr: WasmPtr<ThreadStart<M>, M>,
) -> i32 {
    thread_spawn_internal_from_wasi(&mut ctx, start_ptr)
        .map(|tid| tid as i32)
        .map_err(|errno| errno as i32)
        .unwrap_or_else(|err| -err)
}

/// ### `thread_actions()`
/// Returns the thread start actions to perform.
///
/// Bitfield:
///     1 - Initialize the thread and run its code.
///     2 - Run the thread cleanup code including TLS destructors.
pub fn thread_actions<M: MemorySize + 'static>(ctx: FunctionEnvMut<'_, WasiEnv>) -> i32 {
    thread_actions_internal::<M>(ctx)
}

/// ### `thread_hold()`
/// Holds the current thread after its start function has returned.
///
/// This allow async callbacks to run.
/// Call `thread_release()` to cleanup and end the held thread.
pub fn thread_hold<M: MemorySize + 'static>(
    ctx: FunctionEnvMut<'_, WasiEnv>,
) -> Result<Errno, WasiError> {
    thread_hold_internal::<M>(ctx)
}

/// ### `thread_release()`
/// Cleans up and exits the current thread that has previously been
/// suspended using `thread_hold()`.
pub fn thread_release<M: MemorySize + 'static>(
    ctx: FunctionEnvMut<'_, WasiEnv>,
) -> Result<Errno, WasiError> {
    thread_release_internal::<M>(ctx)
}
