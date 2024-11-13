use super::*;
use crate::syscalls::*;

/// ### `tty_set()`
/// Updates the properties of the rect
#[instrument(level = "trace", skip_all, ret)]
pub fn tty_set<M: MemorySize>(
    _ctx: FunctionEnvMut<'_, WasiEnv>,
    _tty_state: WasmPtr<Tty, M>,
) -> Result<Errno, WasiError> {
    Ok(Errno::Notsup)
}
