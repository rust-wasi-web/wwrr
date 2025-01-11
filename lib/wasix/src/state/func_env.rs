use std::sync::Arc;

use js_sys::Reflect;
use tracing::trace;
use wasm_bindgen::{JsCast, JsValue};
use wasmer::{
    AsStoreMut, AsStoreRef, ExportError, FunctionEnv, Imports, ImportsObj, Instance, Memory,
    Module, Store, Value,
};
use wasmer_wasix_types::wasi::ExitCode;

use crate::{
    import_object_for_all_wasi_versions,
    runtime::SpawnMemoryType,
    state::WasiInstanceHandles,
    utils::{get_wasi_version, get_wasi_versions, store::restore_store_snapshot},
    StoreSnapshot, WasiEnv, WasiError, WasiThreadError,
};

#[derive(Clone, Debug)]
pub struct WasiFunctionEnv {
    pub env: FunctionEnv<WasiEnv>,
}

impl WasiFunctionEnv {
    pub fn new(store: &mut impl AsStoreMut, env: WasiEnv) -> Self {
        Self {
            env: FunctionEnv::new(store, env),
        }
    }

    // Creates a new environment context on a new store.
    pub async fn new_creating_store(
        module: Module,
        env: WasiEnv,
        store_snapshot: Option<&StoreSnapshot>,
        spawn_type: SpawnMemoryType<'_>,
        wbg_js_module: Option<JsValue>,
    ) -> Result<(Self, Store), WasiThreadError> {
        // Create a new store and put the memory object in it
        // (but only if it has imported memory)
        let mut store = env.runtime.new_store();
        let memory = env
            .tasks()
            .build_memory(&mut store.as_store_mut(), spawn_type)?;

        // Build the context object and import the memory
        let mut ctx = WasiFunctionEnv::new(&mut store, env);
        let (mut imports, init) =
            import_object_for_all_wasi_versions(&module, &mut store, &ctx.env);
        if let Some(memory) = memory.clone() {
            imports.define("env", "memory", memory);
        }

        // Get wasm-bindgen imports provided to WebAssembly module.
        let imports_obj = match &wbg_js_module {
            Some(wbg_mod) => {
                tracing::trace!("getting imports for wasm-bindgen");
                let get_imports = Reflect::get(wbg_mod, &JsValue::from_str("__wbg_get_imports"))
                    .map_err(WasiThreadError::wbg_failed)?;
                let get_imports =
                    get_imports
                        .dyn_ref::<js_sys::Function>()
                        .ok_or(WasiThreadError::WbgFailed(
                            "__wbg_get_imports is not a function".to_string(),
                        ))?;
                let imports = get_imports
                    .call0(&JsValue::undefined())
                    .map_err(WasiThreadError::wbg_failed)?;
                ImportsObj(imports.into())
            }
            None => ImportsObj::default(),
        };

        // Instantiate WebAssembly module.
        let instance = Instance::new(&mut store, &module, &imports, imports_obj.clone())
            .await
            .map_err(|err| {
                tracing::warn!("failed to create instance - {}", err);
                WasiThreadError::InstanceCreateFailed(Box::new(err))
            })?;

        // Initialize JavaScript side of wasm-bindgen generated bindings.
        if let Some(wbg_mod) = &wbg_js_module {
            // Set exports provided by WASM module.
            tracing::trace!("setting wasm-bindgen exports");
            let set_exports = Reflect::get(wbg_mod, &JsValue::from_str("__wbg_set_exports"))
                .map_err(WasiThreadError::wbg_failed)?;
            let set_exports =
                set_exports
                    .dyn_ref::<js_sys::Function>()
                    .ok_or(WasiThreadError::WbgFailed(
                        "__wbg_set_exports is not a function".to_string(),
                    ))?;
            set_exports
                .call1(&JsValue::undefined(), &instance.exports_obj.0)
                .map_err(WasiThreadError::wbg_failed)?;

            // Initialize externref table.
            tracing::trace!("initializing wasm-bindgen externref table");
            let wbg = Reflect::get(&imports_obj.0, &JsValue::from_str("wbg"))
                .map_err(WasiThreadError::wbg_failed)?;
            let init_externref_table =
                Reflect::get(&wbg, &JsValue::from_str("__wbindgen_init_externref_table"))
                    .map_err(WasiThreadError::wbg_failed)?;
            let init_externref_table = init_externref_table.dyn_ref::<js_sys::Function>().ok_or(
                WasiThreadError::WbgFailed(
                    "wbg.__wbindgen_init_externref_table is not an exported function".to_string(),
                ),
            )?;
            init_externref_table
                .call0(&JsValue::undefined())
                .map_err(WasiThreadError::wbg_failed)?;
        }

        // Initialize instance.
        init(&instance, &store).map_err(|err| {
            tracing::warn!("failed to init instance - {}", err);
            WasiThreadError::InitFailed(Arc::new(err))
        })?;

        // Initialize the WASI environment
        ctx.initialize_with_memory(&mut store, instance.clone(), memory)
            .map_err(|err| {
                tracing::warn!("failed initialize environment - {}", err);
                WasiThreadError::ExportError(err)
            })?;

        // Set all the globals
        if let Some(snapshot) = store_snapshot {
            restore_store_snapshot(&mut store, snapshot);
        }

        // Enable wait operations on workers.
        if let Ok(wait_prohibited) = instance.exports.get_global("wait_prohibited") {
            wait_prohibited
                .set(&mut store, Value::I32(0))
                .map_err(|err| WasiThreadError::InitFailed(Arc::new(err.into())))?;
        }

        Ok((ctx, store))
    }

    /// Get an `Imports` for a specific version of WASI detected in the module.
    pub fn import_object(
        &self,
        store: &mut impl AsStoreMut,
        module: &Module,
    ) -> Result<Imports, WasiError> {
        let wasi_version = get_wasi_version(module, false).ok_or(WasiError::UnknownWasiVersion)?;
        Ok(crate::generate_import_object_from_env(
            store,
            &self.env,
            wasi_version,
        ))
    }

    /// Gets a reference to the WasiEnvironment
    pub fn data<'a>(&'a self, store: &'a impl AsStoreRef) -> &'a WasiEnv {
        self.env.as_ref(store)
    }

    /// Gets a mutable- reference to the host state in this context.
    pub fn data_mut<'a>(&'a self, store: &'a mut impl AsStoreMut) -> &'a mut WasiEnv {
        self.env.as_mut(store)
    }

    /// Initializes the WasiEnv using the instance exports
    /// (this must be executed before attempting to use it)
    /// (as the stores can not by themselves be passed between threads we can store the module
    ///  in a thread-local variables and use it later - for multithreading)
    pub fn initialize(
        &mut self,
        store: &mut impl AsStoreMut,
        instance: Instance,
    ) -> Result<(), ExportError> {
        self.initialize_with_memory(store, instance, None)
    }

    /// Initializes the WasiEnv using the instance exports and a provided optional memory
    /// (this must be executed before attempting to use it)
    /// (as the stores can not by themselves be passed between threads we can store the module
    ///  in a thread-local variables and use it later - for multithreading)
    pub fn initialize_with_memory(
        &mut self,
        store: &mut impl AsStoreMut,
        instance: Instance,
        memory: Option<Memory>,
    ) -> Result<(), ExportError> {
        let is_wasix_module = crate::utils::is_wasix_module(instance.module());

        let exported_memory = instance
            .exports
            .iter()
            .filter_map(|(_, export)| {
                if let wasmer::Extern::Memory(memory) = export {
                    Some(memory.clone())
                } else {
                    None
                }
            })
            .next();
        let memory = match (exported_memory, memory) {
            (Some(memory), _) => memory,
            (None, Some(memory)) => memory,
            (None, None) => {
                return Err(ExportError::Missing(
                    "No imported or exported memory found".to_string(),
                ))
            }
        };

        let new_inner = WasiInstanceHandles::new(memory, store, instance);

        let env = self.data_mut(store);
        env.set_inner(new_inner);
        env.state.fs.set_is_wasix(is_wasix_module);

        Ok(())
    }

    /// Like `import_object` but containing all the WASI versions detected in
    /// the module.
    pub fn import_object_for_all_wasi_versions(
        &self,
        store: &mut impl AsStoreMut,
        module: &Module,
    ) -> Result<Imports, WasiError> {
        let wasi_versions =
            get_wasi_versions(module, false).ok_or(WasiError::UnknownWasiVersion)?;

        let mut resolver = Imports::new();
        for version in wasi_versions.iter() {
            let new_import_object =
                crate::generate_import_object_from_env(store, &self.env, *version);
            for ((n, m), e) in new_import_object.into_iter() {
                resolver.define(&n, &m, e);
            }
        }

        Ok(resolver)
    }

    /// # Safety
    ///
    /// This function should only be called from within a syscall
    /// as it can potentially execute local thread variable cleanup
    /// code
    pub fn on_exit(&self, store: &mut impl AsStoreMut, exit_code: Option<ExitCode>) {
        trace!(
            "wasi[{}:{}]::on_exit",
            self.data(store).pid(),
            self.data(store).tid()
        );

        // Cleans up all the open files (if this is the main thread)
        self.data(store).blocking_on_exit(exit_code);
    }
}
