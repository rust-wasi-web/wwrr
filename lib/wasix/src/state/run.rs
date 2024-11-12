use wasmer_wasix_types::wasi::ExitCode;

use crate::WasiRuntimeError;

use super::*;

/// Extract the exit code from a `Result<(), WasiRuntimeError>`.
///
/// We need this because calling `exit(0)` inside a WASI program technically
/// triggers [`WasiError`] with an exit code of `0`, but the end user won't want
/// that treated as an error.
pub(super) fn wasi_exit_code(
    mut result: Result<(), WasiRuntimeError>,
) -> (Result<(), WasiRuntimeError>, ExitCode) {
    let exit_code = match &result {
        Ok(_) => Errno::Success.into(),
        Err(err) => match err.as_exit_code() {
            Some(code) if code.is_success() => {
                // This is actually not an error, so we need to fix up the
                // result
                result = Ok(());
                Errno::Success.into()
            }
            Some(other) => other,
            None => Errno::Noexec.into(),
        },
    };

    (result, exit_code)
}
