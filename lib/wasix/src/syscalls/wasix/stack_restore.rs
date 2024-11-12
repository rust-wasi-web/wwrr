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
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    snapshot_ptr: WasmPtr<StackSnapshot, M>,
    mut val: Longsize,
) -> Result<(), WasiError> {
    Err(WasiError::Exit(Errno::Notsup.into()))
}
