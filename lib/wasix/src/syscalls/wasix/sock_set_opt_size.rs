use super::*;
use crate::syscalls::*;

/// ### `sock_set_opt_size()
/// Set size of particular option for this socket
/// Note: This is similar to `setsockopt` in POSIX for SO_RCVBUF
///
/// ## Parameters
///
/// * `fd` - Socket descriptor
/// * `opt` - Socket option to be set
/// * `size` - Buffer size
#[instrument(level = "trace", skip_all, fields(%sock, %opt, %size), ret)]
pub fn sock_set_opt_size(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    sock: WasiFd,
    opt: Sockoption,
    size: Filesize,
) -> Result<Errno, WasiError> {
    wasi_try_ok!(sock_set_opt_size_internal(&mut ctx, sock, opt, size)?);

    Ok(Errno::Success)
}

pub(crate) fn sock_set_opt_size_internal(
    ctx: &mut FunctionEnvMut<'_, WasiEnv>,
    sock: WasiFd,
    opt: Sockoption,
    size: Filesize,
) -> Result<Result<(), Errno>, WasiError> {
    wasi_try_ok_ok!(__sock_actor_mut(
        ctx,
        sock,
        Rights::empty(),
        |mut socket, _| match opt {
            Sockoption::RecvBufSize => socket.set_recv_buf_size(size as usize),
            Sockoption::SendBufSize => socket.set_send_buf_size(size as usize),
            Sockoption::Ttl => socket.set_ttl(size as u32),
            Sockoption::MulticastTtlV4 => socket.set_multicast_ttl_v4(size as u32),
            _ => Err(Errno::Inval),
        }
    ));
    Ok(Ok(()))
}
