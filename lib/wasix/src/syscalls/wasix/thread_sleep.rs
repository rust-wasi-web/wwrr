use virtual_mio::atomics_pause;

use super::*;
use crate::syscalls::*;

/// ### `thread_sleep()`
/// Sends the current thread to sleep for a period of time
///
/// ## Parameters
///
/// * `duration` - Amount of time that the thread should sleep
#[instrument(level = "trace", skip_all, fields(%duration), ret)]
pub fn thread_sleep<M: MemorySize + 'static>(
    ctx: FunctionEnvMut<'_, WasiEnv>,
    duration: Timestamp,
) -> Result<Errno, WasiError> {
    thread_sleep_internal::<M>(ctx, duration)
}

pub(crate) fn thread_sleep_internal<M: MemorySize + 'static>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    duration: Timestamp,
) -> Result<Errno, WasiError> {
    wasi_try_ok!(WasiEnv::process_signals_and_exit(&mut ctx)?);

    let env = ctx.data();

    if duration > 0 {
        let duration = Duration::from_nanos(duration);
        let tasks = env.tasks().clone();
        block_on(async move {
            tasks.sleep_now(duration).await;
        });
    } else {
        atomics_pause();
    }

    Ok(Errno::Success)
}
