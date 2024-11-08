//! The `vm` module re-exports wasmer-vm types.

pub(crate) use crate::js::vm::{
    VMExtern, VMExternFunction, VMExternGlobal, VMExternMemory, VMExternRef, VMExternTable,
    VMFuncRef, VMFunctionCallback, VMFunctionEnvironment, VMInstance, VMTrampoline,
};
pub use crate::js::vm::{VMFunction, VMGlobal, VMMemory, VMSharedMemory, VMTable};

// Deprecated exports
pub use wasmer_types::{MemoryError, MemoryStyle, TableStyle};
