use super::*;

#[allow(clippy::extra_unused_type_parameters)]
#[deprecated="journal stuff"]
pub fn maybe_snapshot<M: MemorySize>(
    ctx: FunctionEnvMut<'_, WasiEnv>,
) -> WasiResult<FunctionEnvMut<'_, WasiEnv>> {
    Ok(Ok(ctx))
}

