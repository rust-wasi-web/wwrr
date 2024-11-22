//! Execute code from a running WebAssembly instance on another thread.

#![allow(clippy::borrowed_box)] // Generated by derivative

use anyhow::Context;
use derivative::Derivative;
use js_sys::WebAssembly;
use wasm_bindgen::{JsCast, JsValue};
use wasmer::{AsJs, AsStoreRef, Memory, MemoryType, Store};
use wasmer_wasix::{
    runtime::{
        task_manager::{TaskWasm, TaskWasmRun, TaskWasmRunProperties},
        SpawnMemoryType,
    },
    StoreSnapshot, WasiEnv, WasiFunctionEnv, WasiThreadError,
};

use crate::tasks::SchedulerMessage;

pub(crate) fn to_scheduler_message(
    task: TaskWasm<'_, '_>,
) -> Result<SchedulerMessage, WasiThreadError> {
    let TaskWasm {
        run,
        env,
        module,
        spawn_type,
        globals,
    } = task;

    let (memory_ty, memory, run_type) = match spawn_type {
        wasmer_wasix::runtime::SpawnMemoryType::CreateMemory => {
            (None, None, WasmMemoryType::CreateMemory)
        }
        wasmer_wasix::runtime::SpawnMemoryType::CreateMemoryOfType(ty) => {
            (Some(ty), None, WasmMemoryType::CreateMemoryOfType(ty))
        }
        wasmer_wasix::runtime::SpawnMemoryType::CopyMemory(m, store) => {
            let memory_ty = m.ty(&store);
            let memory = m.as_jsvalue(&store);

            // We copy the memory here rather than later as
            // the fork syscalls need to copy the memory
            // synchronously before the next thread accesses
            // and before the fork parent resumes, otherwise
            // there will be memory corruption
            let memory = copy_memory(&memory, m.ty(&store))?;

            (
                Some(memory_ty),
                Some(memory),
                WasmMemoryType::ShareMemory(memory_ty),
            )
        }
        wasmer_wasix::runtime::SpawnMemoryType::ShareMemory(m, store) => {
            let ty = m.ty(&store);
            let memory = m.as_jsvalue(&store);
            (
                Some(ty),
                Some(memory),
                WasmMemoryType::ShareMemory(m.ty(&store)),
            )
        }
    };

    let memory = memory.map(|m| {
        // HACK: The store isn't used when converting memories, so it's fine to
        // use a dummy one.
        let mut store = wasmer::Store::default();
        let ty = memory_ty.expect("Guaranteed to be set");
        match wasmer::Memory::from_jsvalue(&mut store, &ty, &m) {
            Ok(m) => m,
            Err(_) => unreachable!(),
        }
    });

    let store_snapshot = globals.cloned();
    let spawn_wasm = SpawnWasm {
        run,
        run_type,
        env,
        store_snapshot,
    };

    Ok(SchedulerMessage::SpawnWithModuleAndMemory {
        module,
        memory,
        spawn_wasm,
    })
}

#[derive(Debug, Clone)]
pub(crate) enum WasmMemoryType {
    CreateMemory,
    CreateMemoryOfType(MemoryType),
    ShareMemory(MemoryType),
}

/// Duplicate a [`WebAssembly::Memory`] instance.
fn copy_memory(memory: &JsValue, ty: MemoryType) -> Result<JsValue, WasiThreadError> {
    let memory_js = memory.dyn_ref::<WebAssembly::Memory>().unwrap();

    let descriptor = js_sys::Object::new();

    // Annotation is here to prevent spurious IDE warnings.
    js_sys::Reflect::set(&descriptor, &"initial".into(), &ty.minimum.0.into()).unwrap();
    if let Some(max) = ty.maximum {
        js_sys::Reflect::set(&descriptor, &"maximum".into(), &max.0.into()).unwrap();
    }
    js_sys::Reflect::set(&descriptor, &"shared".into(), &ty.shared.into()).unwrap();

    let new_memory = WebAssembly::Memory::new(&descriptor).map_err(|_e| {
        WasiThreadError::MemoryCreateFailed(wasmer::MemoryError::Generic(
            "Error while creating the memory".to_owned(),
        ))
    })?;

    let src_buffer = memory_js.buffer();
    let src_size: u64 = src_buffer
        .unchecked_ref::<js_sys::ArrayBuffer>()
        .byte_length()
        .into();
    let src_view = js_sys::Uint8Array::new(&src_buffer);

    let pages = ((src_size as usize - 1) / wasmer::WASM_PAGE_SIZE) + 1;
    new_memory.grow(pages as u32);

    let dst_buffer = new_memory.buffer();
    let dst_view = js_sys::Uint8Array::new(&dst_buffer);

    tracing::trace!(src_size, "memory copy started");

    {
        let mut offset = 0_u64;
        let mut chunk = [0u8; 40960];
        while offset < src_size {
            let remaining = src_size - offset;
            let sublen = remaining.min(chunk.len() as u64);
            let end = offset.checked_add(sublen).unwrap();
            src_view
                .subarray(offset.try_into().unwrap(), end.try_into().unwrap())
                .copy_to(&mut chunk[..sublen as usize]);
            dst_view
                .subarray(offset.try_into().unwrap(), end.try_into().unwrap())
                .copy_from(&chunk[..sublen as usize]);
            offset += sublen;
        }
    }

    Ok(new_memory.into())
}

#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) struct SpawnWasm {
    /// A blocking callback to run.
    #[derivative(Debug(format_with = "crate::utils::hidden"))]
    run: Box<TaskWasmRun>,
    /// How the memory should be instantiated to execute the [`SpawnWasm::run`]
    /// callback.
    run_type: WasmMemoryType,
    /// WASI environment,
    pub(crate) env: WasiEnv,
    /// A snapshot of the instance store, used to fork from existing instances.
    store_snapshot: Option<StoreSnapshot>,
}

impl SpawnWasm {
    pub(crate) fn shared_memory_type(&self) -> Option<MemoryType> {
        match self.run_type {
            WasmMemoryType::ShareMemory(ty) => Some(ty),
            WasmMemoryType::CreateMemory | WasmMemoryType::CreateMemoryOfType(_) => None,
        }
    }

    /// Prepare the WebAssembly task for execution, waiting for any triggers to
    /// resolve.
    pub(crate) async fn begin(self) -> ReadySpawnWasm {
        ReadySpawnWasm(self)
    }
}

/// A [`SpawnWasm`] instance that is ready to be executed.
#[derive(Debug)]
pub struct ReadySpawnWasm(SpawnWasm);

impl ReadySpawnWasm {
    /// Execute the callback, blocking until it has completed.
    pub(crate) async fn execute(
        self,
        wasm_module: wasmer::Module,
        wasm_memory: JsValue,
        wbg_js_module: Option<JsValue>,
    ) -> Result<(), anyhow::Error> {
        let ReadySpawnWasm(SpawnWasm {
            run,
            run_type,
            env,
            store_snapshot,
        }) = self;

        // Invoke the callback which will run the web assembly module
        let (ctx, store) = build_ctx_and_store(
            wasm_module,
            wasm_memory,
            env,
            store_snapshot,
            run_type,
            wbg_js_module.clone(),
        )
        .context("Unable to initialize the context and store")?;

        let properties = TaskWasmRunProperties {
            ctx,
            store,
            wbg_js_module,
        };
        run(properties).await;

        Ok(())
    }
}

fn build_ctx_and_store(
    module: wasmer::Module,
    memory: JsValue,
    env: WasiEnv,
    store_snapshot: Option<StoreSnapshot>,
    run_type: WasmMemoryType,
    wbg_js_module: Option<JsValue>,
) -> Option<(WasiFunctionEnv, Store)> {
    // Make a fake store which will hold the memory we just transferred
    let mut temp_store = env.runtime().new_store();
    let spawn_type = match run_type {
        WasmMemoryType::CreateMemory => SpawnMemoryType::CreateMemory,
        WasmMemoryType::CreateMemoryOfType(mem) => SpawnMemoryType::CreateMemoryOfType(mem),
        WasmMemoryType::ShareMemory(ty) => {
            let memory = match Memory::from_jsvalue(&mut temp_store, &ty, &memory) {
                Ok(a) => a,
                Err(_) => {
                    tracing::error!("Failed to receive memory for module");
                    return None;
                }
            };
            SpawnMemoryType::ShareMemory(memory, temp_store.as_store_ref())
        }
    };

    let (ctx, store) = match WasiFunctionEnv::new_creating_store(
        module,
        env,
        store_snapshot.as_ref(),
        spawn_type,
        wbg_js_module,
    ) {
        Ok(a) => a,
        Err(err) => {
            tracing::error!(
                error = &err as &dyn std::error::Error,
                "Failed to crate wasi context",
            );
            return None;
        }
    };
    Some((ctx, store))
}
