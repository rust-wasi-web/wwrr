use super::*;

#[allow(clippy::extra_unused_type_parameters)]
#[deprecated = "journal stuff"]
pub fn maybe_snapshot_once<M: MemorySize>(
    ctx: FunctionEnvMut<'_, WasiEnv>,
    _trigger: crate::journal::SnapshotTrigger,
) -> WasiResult<FunctionEnvMut<'_, WasiEnv>> {
    Ok(Ok(ctx))
}
