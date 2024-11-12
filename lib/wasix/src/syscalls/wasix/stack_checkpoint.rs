use super::*;
use crate::syscalls::*;

/// ### `stack_checkpoint()`
/// Creates a snapshot of the current stack which allows it to be restored
/// later using its stack hash.
#[instrument(level = "trace", skip_all, ret)]
pub fn stack_checkpoint<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    snapshot_ptr: WasmPtr<StackSnapshot, M>,
    ret_val: WasmPtr<Longsize, M>,
) -> Result<Errno, WasiError> {
    Ok(Errno::Notsup)
}
