use super::*;
use crate::syscalls::*;

/// ### `fd_advise()`
/// Advise the system about how a file will be used
/// Inputs:
/// - `Fd fd`
///     The file descriptor the advice applies to
/// - `Filesize offset`
///     The offset from which the advice applies
/// - `Filesize len`
///     The length from the offset to which the advice applies
/// - `__wasi_advice_t advice`
///     The advice to give
#[instrument(level = "trace", skip_all, fields(%fd, %offset, %len, ?advice), ret)]
pub fn fd_advise(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    fd: WasiFd,
    offset: Filesize,
    len: Filesize,
    advice: Advice,
) -> Result<Errno, WasiError> {
    wasi_try_ok!(fd_advise_internal(&mut ctx, fd, offset, len, advice));
    Ok(Errno::Success)
}

pub(crate) fn fd_advise_internal(
    ctx: &mut FunctionEnvMut<'_, WasiEnv>,
    fd: WasiFd,
    offset: Filesize,
    len: Filesize,
    _advice: Advice,
) -> Result<(), Errno> {
    // Instead of unconditionally returning OK.  This barebones implementation
    // only performs basic fd and rights checks.

    let env = ctx.data();
    let (_, state) = unsafe { env.get_memory_and_wasi_state(&ctx, 0) };
    let fd_entry = state.fs.get_fd(fd)?;
    let _inode = fd_entry.inode;

    if !fd_entry.rights.contains(Rights::FD_ADVISE) {
        return Err(Errno::Access);
    }

    let _end = offset.checked_add(len).ok_or(Errno::Inval)?;

    Ok(())
}
