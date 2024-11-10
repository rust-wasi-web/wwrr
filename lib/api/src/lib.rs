#![deny(
    missing_docs,
    trivial_numeric_casts,
    unused_extern_crates,
    rustdoc::broken_intra_doc_links
)]
#![warn(unused_import_braces)]
#![allow(clippy::new_without_default, clippy::vtable_address_comparisons)]
#![warn(
    clippy::float_arithmetic,
    clippy::mut_mut,
    clippy::nonminimal_bool,
    clippy::map_unwrap_or,
    clippy::print_stdout,
    clippy::unicode_not_nfc,
    clippy::use_self
)]
#![crate_type = "cdylib"]

//! [`Wasmer`](https://wasmer.io/) is the most popular
//! [WebAssembly](https://webassembly.org/) runtime for Rust. It supports
//! JIT (Just In Time) and AOT (Ahead Of Time) compilation as well as
//! pluggable compilers suited to your needs.
//!

#[cfg(not(target_arch = "wasm32"))]
compile_error!("target architecture must be `wasm32`");

mod access;
mod engine;
mod errors;
mod exports;
mod extern_ref;
mod externals;
mod function_env;
mod imports;
mod instance;
mod into_bytes;
mod mem_access;
mod module;
mod native_type;
mod ptr;
mod store;
mod typed_function;
mod value;
pub mod vm;

mod module_info_polyfill;

mod js;
pub use js::*;

pub use crate::externals::{
    Extern, Function, Global, HostFunction, Memory, MemoryLocation, MemoryView, SharedMemory, Table,
};
pub use access::WasmSliceAccess;
pub use engine::{AsEngineRef, Engine, EngineRef};
pub use errors::{AtomicsError, InstantiationError, LinkError, RuntimeError};
pub use exports::{ExportError, Exportable, Exports, ExportsIterator};
pub use extern_ref::ExternRef;
pub use function_env::{FunctionEnv, FunctionEnvMut};
pub use imports::Imports;
pub use instance::Instance;
pub use into_bytes::IntoBytes;
pub use mem_access::{MemoryAccessError, WasmRef, WasmSlice, WasmSliceIter};
pub use module::{IoCompileError, Module};
pub use native_type::{FromToNativeWasmType, NativeWasmTypeInto, WasmTypeList};
pub use ptr::{Memory32, Memory64, MemorySize, WasmPtr, WasmPtr64};
pub use store::{
    AsStoreMut, AsStoreRef, OnCalledHandler, Store, StoreId, StoreMut, StoreObjects, StoreRef,
};
pub use typed_function::TypedFunction;
pub use value::Value;

// Reexport from other modules

pub use wasmer_derive::ValueType;
// TODO: OnCalledAction is needed for asyncify. It will be refactored with https://github.com/wasmerio/wasmer/issues/3451
pub use wasmer_types::{
    is_wasm, Bytes, CompileError, CpuFeature, DeserializeError, ExportIndex, ExportType,
    ExternType, FrameInfo, FunctionType, GlobalInit, GlobalType, ImportType, LocalFunctionIndex,
    MemoryError, MemoryType, MiddlewareError, Mutability, OnCalledAction, Pages,
    ParseCpuFeatureError, SerializeError, TableType, Target, Type, ValueType, WasmError,
    WasmResult, WASM_MAX_PAGES, WASM_MIN_PAGES, WASM_PAGE_SIZE,
};
#[cfg(feature = "wat")]
pub use wat::parse_bytes as wat2wasm;

pub use wasmparser;

/// Version number of this crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
