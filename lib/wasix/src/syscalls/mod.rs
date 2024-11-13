#![allow(clippy::too_many_arguments, clippy::cognitive_complexity)]

pub mod types {
    pub use wasmer_wasix_types::{types::*, wasi};
}

pub mod wasi;
pub mod wasix;
pub mod wasm;

use futures::future;
use futures::Future;
pub use wasi::*;
pub use wasix::*;
use wasmer_wasix_types::wasix::ThreadStartType;

pub mod legacy;

pub(crate) use std::{
    borrow::{Borrow, Cow},
    collections::{HashMap, HashSet},
    convert::TryInto,
    io::Read,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::{Deref, DerefMut},
    path::Path,
    pin::Pin,
    sync::{atomic::Ordering, Arc},
    task::{Context, Poll},
    time::Duration,
};

pub(crate) use cooked_waker::IntoWaker;
pub(crate) use tracing::{debug, error, trace, warn};
pub use wasm::*;

pub(crate) use virtual_fs::{AsyncSeekExt, AsyncWriteExt, FileSystem, FsError};
pub(crate) use virtual_net::StreamSecurity;
pub(crate) use wasmer::{
    AsStoreMut, AsStoreRef, Function, FunctionEnvMut, Memory32, Memory64, MemorySize, MemoryView,
    Store, WasmPtr, WasmSlice,
};
pub(crate) use wasmer_wasix_types::wasi::EventUnion;

pub(crate) use self::types::{
    wasi::{
        Addressfamily, Advice, Clockid, Dircookie, Dirent, Errno, Event, EventFdReadwrite,
        Eventtype, ExitCode, Fd as WasiFd, Fdflags, Fdstat, Filesize, Filestat, Filetype, Fstflags,
        Linkcount, Longsize, Pid, Prestat, Rights, Snapshot0Clockid, Sockoption, Sockstatus,
        Socktype, StackSnapshot, Streamsecurity, Subscription, SubscriptionFsReadwrite, Tid,
        Timestamp, Tty, Whence,
    },
    *,
};
use self::{state::conv_env_vars, utils::WasiDummyWaker};
pub(crate) use crate::net::net_error_into_wasi_err;
pub(crate) use crate::os::task::{process::WasiProcessId, thread::WasiThreadId};
use crate::{
    fs::{fs_error_into_wasi_err, virtual_file_type_to_wasi_file_type, Fd, Kind},
    runtime::task_manager::InlineWaker,
    WasiResult,
};
pub(crate) use crate::{
    mem_error_to_wasi,
    net::{
        read_ip_port,
        socket::{InodeSocket, InodeSocketKind},
        write_ip_port,
    },
    state::{self, InodeGuard, PollEvent, PollEventBuilder, WasiState},
    utils::{self, map_io_err},
    VirtualTaskManager, WasiEnv, WasiError, WasiFunctionEnv,
};

pub(crate) fn to_offset<M: MemorySize>(offset: usize) -> Result<M::Offset, Errno> {
    let ret: M::Offset = offset.try_into().map_err(|_| Errno::Inval)?;
    Ok(ret)
}

pub(crate) fn from_offset<M: MemorySize>(offset: M::Offset) -> Result<usize, Errno> {
    let ret: usize = offset.try_into().map_err(|_| Errno::Inval)?;
    Ok(ret)
}

pub(crate) fn copy_from_slice<M: MemorySize>(
    mut read_loc: &[u8],
    memory: &MemoryView,
    iovs_arr: WasmSlice<__wasi_iovec_t<M>>,
) -> Result<usize, Errno> {
    let mut bytes_read = 0usize;

    let iovs_arr = iovs_arr.access().map_err(mem_error_to_wasi)?;
    for iovs in iovs_arr.iter() {
        let mut buf = WasmPtr::<u8, M>::new(iovs.buf)
            .slice(memory, iovs.buf_len)
            .map_err(mem_error_to_wasi)?
            .access()
            .map_err(mem_error_to_wasi)?;

        let to_read = from_offset::<M>(iovs.buf_len)?;
        let to_read = to_read.min(read_loc.len());
        if to_read == 0 {
            break;
        }
        let (left, right) = read_loc.split_at(to_read);
        let amt = buf.copy_from_slice_min(left);
        if amt != to_read {
            return Ok(bytes_read + amt);
        }

        read_loc = right;
        bytes_read += to_read;
    }
    Ok(bytes_read)
}

pub(crate) fn read_bytes<T: Read, M: MemorySize>(
    mut reader: T,
    memory: &MemoryView,
    iovs_arr: WasmSlice<__wasi_iovec_t<M>>,
) -> Result<usize, Errno> {
    let mut bytes_read = 0usize;

    let iovs_arr = iovs_arr.access().map_err(mem_error_to_wasi)?;
    for iovs in iovs_arr.iter() {
        let mut buf = WasmPtr::<u8, M>::new(iovs.buf)
            .slice(memory, iovs.buf_len)
            .map_err(mem_error_to_wasi)?
            .access()
            .map_err(mem_error_to_wasi)?;

        let to_read = buf.len();
        let has_read = reader.read(buf.as_mut()).map_err(map_io_err)?;

        bytes_read += has_read;
        if has_read != to_read {
            return Ok(bytes_read);
        }
    }
    Ok(bytes_read)
}

/// Blocks the thread on the specified Future.
pub(crate) fn block_on<T, Fut>(work: Fut) -> T
where
    T: 'static,
    Fut: Future<Output = T>,
{
    InlineWaker::block_on(work)
}

/// Blocks the thread on the specified Future with a timeout.
///
/// If the timeout is reached, [`Errno::Timedout`] is returned.
pub(crate) fn block_on_with_timeout<T, Fut>(
    env: &WasiEnv,
    timeout: Option<Duration>,
    work: Fut,
) -> Result<T, Errno>
where
    T: 'static,
    Fut: Future<Output = Result<T, Errno>>,
{
    let timeout_task = async {
        match timeout {
            Some(timeout) => env.tasks().sleep_now(timeout).await,
            None => future::pending().await,
        }
    };

    let task = async {
        tokio::select! {
            res = work => res,
            () = timeout_task => Err(Errno::Timedout)
        }
    };

    InlineWaker::block_on(task)
}

/// Executes the specified future on the current thread while handling signals
/// and terminaiton.
///
/// If timeout is zero and future would block, [`Errno::Again`] is returned.
pub(crate) fn block_on_with_signals<T, Fut>(
    ctx: &mut FunctionEnvMut<'_, WasiEnv>,
    timeout: Option<Duration>,
    work: Fut,
) -> WasiResult<T>
where
    T: 'static,
    Fut: std::future::Future<Output = Result<T, Errno>>,
{
    let env = ctx.data();

    // Check if we need to exit the asynchronous loop
    if let Some(exit_code) = env.should_exit() {
        return Err(WasiError::Exit(exit_code));
    }

    // This poller will process any signals when the main working function is idle
    struct SignalPoller<'a, 'b, Fut, T>
    where
        Fut: Future<Output = Result<T, Errno>>,
    {
        ctx: &'a mut FunctionEnvMut<'b, WasiEnv>,
        pinned_work: Pin<Box<Fut>>,
    }

    impl<'a, 'b, Fut, T> Future for SignalPoller<'a, 'b, Fut, T>
    where
        Fut: Future<Output = Result<T, Errno>>,
    {
        type Output = Result<Fut::Output, WasiError>;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            if let Poll::Ready(res) = Pin::new(&mut self.pinned_work).poll(cx) {
                return Poll::Ready(Ok(res));
            }
            if let Some(signals) = self.ctx.data().thread.pop_signals_or_subscribe(cx.waker()) {
                if let Err(err) = WasiEnv::process_signals_internal(self.ctx, signals) {
                    return Poll::Ready(Err(err));
                }
                return Poll::Ready(Ok(Err(Errno::Intr)));
            }
            Poll::Pending
        }
    }

    // Block on the work
    let pinned_work = Box::pin(work);
    let tasks = env.tasks().clone();
    let poller = SignalPoller { ctx, pinned_work };

    // Non-blocking path.
    if let Some(Duration::ZERO) = timeout {
        let waker = WasiDummyWaker.into_waker();
        let mut cx = Context::from_waker(&waker);
        let mut pinned_work = Box::pin(poller);
        match pinned_work.as_mut().poll(&mut cx) {
            Poll::Ready(res) => return res,
            Poll::Pending => return Ok(Err(Errno::Again)),
        }
    }

    let timeout = async move {
        match timeout {
            Some(timeout) => tasks.sleep_now(timeout).await,
            None => future::pending().await,
        }
    };

    // Block on the work with timeout.
    let task = async move {
        tokio::select! {
            res = poller => res,
            () = timeout => Ok(Err(Errno::Timedout)),
        }
    };

    InlineWaker::block_on(task)
}

/// Performs an immutable operation on the socket.
pub(crate) fn block_on_sock<T, F, Fut>(
    env: &WasiEnv,
    sock: WasiFd,
    rights: Rights,
    actor: F,
) -> Result<T, Errno>
where
    F: FnOnce(crate::net::socket::InodeSocket, Fd) -> Fut,
    Fut: std::future::Future<Output = Result<T, Errno>>,
{
    let fd_entry = env.state.fs.get_fd(sock)?;
    if !rights.is_empty() && !fd_entry.rights.contains(rights) {
        return Err(Errno::Access);
    }

    let work = {
        let inode = fd_entry.inode.clone();
        let mut guard = inode.write();
        match guard.deref_mut() {
            Kind::Socket { socket } => {
                // Clone the socket and release the lock
                let socket = socket.clone();
                drop(guard);

                // Start the work using the socket
                actor(socket, fd_entry)
            }
            _ => {
                return Err(Errno::Notsock);
            }
        }
    };

    InlineWaker::block_on(work)
}

/// Performs an immutable operation on the socket while running in an asynchronous runtime
/// This has built in signal support
pub(crate) fn __sock_actor<T, F>(
    ctx: &mut FunctionEnvMut<'_, WasiEnv>,
    sock: WasiFd,
    rights: Rights,
    actor: F,
) -> Result<T, Errno>
where
    T: 'static,
    F: FnOnce(crate::net::socket::InodeSocket, Fd) -> Result<T, Errno>,
{
    let env = ctx.data();

    let fd_entry = env.state.fs.get_fd(sock)?;
    if !rights.is_empty() && !fd_entry.rights.contains(rights) {
        return Err(Errno::Access);
    }

    let inode = fd_entry.inode.clone();

    let mut guard = inode.write();
    match guard.deref_mut() {
        Kind::Socket { socket } => {
            // Clone the socket and release the lock
            let socket = socket.clone();
            drop(guard);

            // Start the work using the socket
            actor(socket, fd_entry)
        }
        _ => Err(Errno::Notsock),
    }
}

/// Performs mutable work on a socket under an asynchronous runtime with
/// built in signal processing
pub(crate) fn __sock_actor_mut<T, F>(
    ctx: &mut FunctionEnvMut<'_, WasiEnv>,
    sock: WasiFd,
    rights: Rights,
    actor: F,
) -> Result<T, Errno>
where
    T: 'static,
    F: FnOnce(crate::net::socket::InodeSocket, Fd) -> Result<T, Errno>,
{
    let env = ctx.data();

    let fd_entry = env.state.fs.get_fd(sock)?;
    if !rights.is_empty() && !fd_entry.rights.contains(rights) {
        return Err(Errno::Access);
    }

    let inode = fd_entry.inode.clone();
    let mut guard = inode.write();
    match guard.deref_mut() {
        Kind::Socket { socket } => {
            // Clone the socket and release the lock
            let socket = socket.clone();
            drop(guard);

            // Start the work using the socket
            actor(socket, fd_entry)
        }
        _ => Err(Errno::Notsock),
    }
}

/// Replaces a socket with another socket in under an asynchronous runtime.
/// This is used for opening sockets or connecting sockets which changes
/// the fundamental state of the socket to another state machine
pub(crate) fn __sock_upgrade<'a, F, Fut>(
    ctx: &'a mut FunctionEnvMut<'_, WasiEnv>,
    sock: WasiFd,
    rights: Rights,
    actor: F,
) -> Result<(), Errno>
where
    F: FnOnce(crate::net::socket::InodeSocket, Fdflags) -> Fut,
    Fut: std::future::Future<Output = Result<Option<crate::net::socket::InodeSocket>, Errno>> + 'a,
{
    let env = ctx.data();
    let fd_entry = env.state.fs.get_fd(sock)?;
    if !rights.is_empty() && !fd_entry.rights.contains(rights) {
        tracing::warn!(
            "wasi[{}:{}]::sock_upgrade(fd={}, rights={:?}) - failed - no access rights to upgrade",
            ctx.data().pid(),
            ctx.data().tid(),
            sock,
            rights
        );
        return Err(Errno::Access);
    }

    let inode = fd_entry.inode;
    let mut guard = inode.write();
    match guard.deref_mut() {
        Kind::Socket { socket } => {
            let socket = socket.clone();
            drop(guard);

            // Start the work using the socket
            let work = actor(socket, fd_entry.flags);

            // Block on the work and process it
            let res = InlineWaker::block_on(work);
            let new_socket = res?;

            if let Some(mut new_socket) = new_socket {
                let mut guard = inode.write();
                match guard.deref_mut() {
                    Kind::Socket { socket, .. } => {
                        std::mem::swap(socket, &mut new_socket);
                    }
                    _ => {
                        tracing::warn!(
                            "wasi[{}:{}]::sock_upgrade(fd={}, rights={:?}) - failed - not a socket",
                            ctx.data().pid(),
                            ctx.data().tid(),
                            sock,
                            rights
                        );
                        return Err(Errno::Notsock);
                    }
                }
            }
        }
        _ => {
            tracing::warn!(
                "wasi[{}:{}]::sock_upgrade(fd={}, rights={:?}) - failed - not a socket",
                ctx.data().pid(),
                ctx.data().tid(),
                sock,
                rights
            );
            return Err(Errno::Notsock);
        }
    }

    Ok(())
}

#[must_use]
pub(crate) fn write_buffer_array<M: MemorySize>(
    memory: &MemoryView,
    from: &[Vec<u8>],
    ptr_buffer: WasmPtr<WasmPtr<u8, M>, M>,
    buffer: WasmPtr<u8, M>,
) -> Errno {
    let ptrs = wasi_try_mem!(ptr_buffer.slice(memory, wasi_try!(to_offset::<M>(from.len()))));

    let mut current_buffer_offset = 0usize;
    for (sub_buffer, ptr) in from.iter().zip(ptrs.iter()) {
        let mut buf_offset = buffer.offset();
        buf_offset += wasi_try!(to_offset::<M>(current_buffer_offset));
        let new_ptr = WasmPtr::new(buf_offset);
        wasi_try_mem!(ptr.write(new_ptr));

        let data =
            wasi_try_mem!(new_ptr.slice(memory, wasi_try!(to_offset::<M>(sub_buffer.len()))));
        wasi_try_mem!(data.write_slice(sub_buffer));
        wasi_try_mem!(wasi_try_mem!(
            new_ptr.add_offset(wasi_try!(to_offset::<M>(sub_buffer.len())))
        )
        .write(memory, 0));

        current_buffer_offset += sub_buffer.len() + 1;
    }

    Errno::Success
}

pub(crate) fn get_current_time_in_nanos() -> Result<Timestamp, Errno> {
    let now = platform_clock_time_get(Snapshot0Clockid::Monotonic, 1_000_000).unwrap() as u128;
    Ok(now as Timestamp)
}

// Function to prepare the WASI environment
pub(crate) fn _prepare_wasi(
    wasi_env: &mut WasiEnv,
    args: Option<Vec<String>>,
    envs: Option<Vec<(String, String)>>,
) {
    // Swap out the arguments with the new ones
    if let Some(args) = args {
        let mut wasi_state = wasi_env.state.fork();
        wasi_state.args = args;
        wasi_env.state = Arc::new(wasi_state);
    }

    // Update the env vars
    if let Some(envs) = envs {
        let mut guard = wasi_env.state.envs.lock().unwrap();

        let mut existing_envs = guard
            .iter()
            .map(|b| {
                let string = String::from_utf8_lossy(b);
                let (key, val) = string.split_once('=').expect("env var is malformed");

                (key.to_string(), val.to_string().as_bytes().to_vec())
            })
            .collect::<Vec<_>>();

        for (key, val) in envs {
            let val = val.as_bytes().to_vec();
            match existing_envs
                .iter_mut()
                .find(|(existing_key, _)| existing_key == &key)
            {
                Some((_, existing_val)) => *existing_val = val,
                None => existing_envs.push((key, val)),
            }
        }

        let envs = conv_env_vars(existing_envs);

        *guard = envs;

        drop(guard)
    }

    // Close any files after the STDERR that are not preopened
    let close_fds = {
        let preopen_fds = {
            let preopen_fds = wasi_env.state.fs.preopen_fds.read().unwrap();
            preopen_fds.iter().copied().collect::<HashSet<_>>()
        };
        let fd_map = wasi_env.state.fs.fd_map.read().unwrap();
        fd_map
            .keys()
            .filter_map(|a| match *a {
                a if a <= __WASI_STDERR_FILENO => None,
                a if preopen_fds.contains(&a) => None,
                a => Some(a),
            })
            .collect::<Vec<_>>()
    };

    // Now close all these files
    for fd in close_fds {
        let _ = wasi_env.state.fs.close_fd(fd);
    }
}
