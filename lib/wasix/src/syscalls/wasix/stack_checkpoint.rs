use super::*;
use crate::syscalls::*;

/// ### `stack_checkpoint()`
/// Creates a snapshot of the current stack which allows it to be restored
/// later using its stack hash.
#[instrument(level = "trace", skip_all, ret)]
pub fn stack_checkpoint<M: MemorySize>(
    _ctx: FunctionEnvMut<'_, WasiEnv>,
    _snapshot_ptr: WasmPtr<StackSnapshot, M>,
    _ret_val: WasmPtr<Longsize, M>,
) -> Result<Errno, WasiError> {
    Ok(Errno::Notsup)
}
