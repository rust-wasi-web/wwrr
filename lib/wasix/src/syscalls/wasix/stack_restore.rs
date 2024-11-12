use super::*;
use crate::syscalls::*;

/// ### `stack_restore()`
/// Restores the current stack to a previous stack described by its
/// stack hash.
///
/// ## Parameters
///
/// * `snapshot_ptr` - Contains a previously made snapshot
#[instrument(level = "trace", skip_all, ret)]
pub fn stack_restore<M: MemorySize>(
    _ctx: FunctionEnvMut<'_, WasiEnv>,
    _snapshot_ptr: WasmPtr<StackSnapshot, M>,
    _val: Longsize,
) -> Result<(), WasiError> {
    Err(WasiError::Exit(Errno::Notsup.into()))
}
