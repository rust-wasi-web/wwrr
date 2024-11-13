use super::*;
use crate::syscalls::*;

/// ### `tty_get()`
/// Retrieves the current state of the TTY
#[instrument(level = "trace", skip_all, ret)]
pub fn tty_get<M: MemorySize>(
    _ctx: FunctionEnvMut<'_, WasiEnv>,
    _tty_state: WasmPtr<Tty, M>,
) -> Errno {
    Errno::Notsup
}
