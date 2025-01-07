use std::sync::Arc;
use utils::Error;

use wasm_bindgen::{prelude::*, JsCast};
use wasmer::ImportsObj;
use wasmer_wasix::{WasiEnvBuilder, WasiReactor};

use crate::RunOptions;

const DEFAULT_PROGRAM_NAME: &str = "wasm";

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(typescript_type = "Uint8Array")]
    pub type WasmModule;
}

impl WasmModule {
    async fn to_module(
        &self,
        runtime: &dyn wasmer_wasix::Runtime,
    ) -> Result<wasmer::Module, Error> {
        let Some(buffer) = self.dyn_ref::<js_sys::Uint8Array>() else {
            unreachable!()
        };

        Ok(runtime.load_module(&buffer.to_vec()).await?)
    }
}

/// A handle connected to a loaded WASIX program.
#[derive(Debug)]
#[wasm_bindgen]
pub struct WasiReactorInstance {
    /// WASI reactor instance.
    reactor: Arc<WasiReactor>,
    /// The standard input stream, if one wasn't provided when starting the
    /// instance.
    #[wasm_bindgen(getter_with_clone, readonly)]
    pub stdin: Option<web_sys::WritableStream>,
    /// The WASI program's standard output.
    #[wasm_bindgen(getter_with_clone, readonly)]
    pub stdout: web_sys::ReadableStream,
    /// The WASI program's standard error.
    #[wasm_bindgen(getter_with_clone, readonly)]
    pub stderr: web_sys::ReadableStream,
}

#[wasm_bindgen]
impl WasiReactorInstance {
    /// Exports from the WASM module.
    #[wasm_bindgen(getter)]
    pub fn exports(&self) -> js_sys::Object {
        self.reactor.exports_obj().0
    }
}

/// Loads a WASIX program.
#[wasm_bindgen(js_name = "loadWasix")]
pub async fn load_wasix(
    wasm_module: WasmModule,
    config: RunOptions,
    imports_obj: js_sys::Object,
    wbg_js_module_url: String,
) -> Result<WasiReactorInstance, Error> {
    // Check whehther our memory is shared, so that we can start web workers.
    let our_memory = wasm_bindgen::memory();
    if !is_memory_shared(our_memory.dyn_ref().unwrap()) {
        return Err(anyhow::anyhow!("wwrr memory is not shared").into());
    }
    let runtime = config.runtime().resolve()?.into_inner();

    let program_name = config
        .program()
        .as_string()
        .unwrap_or_else(|| DEFAULT_PROGRAM_NAME.to_string());

    let mut builder = WasiEnvBuilder::new(program_name).runtime(runtime.clone());
    let (stdin, stdout, stderr) = config.configure_builder(&mut builder)?;

    let module: wasmer::Module = wasm_module.to_module(&*runtime).await?;

    tracing::info!("loading with module: {module:?} and JavaScript bindings {wbg_js_module_url}");
    builder.set_wbg_js_module_name(wbg_js_module_url);

    let _span = tracing::debug_span!("load").entered();

    let imports_obj = ImportsObj(imports_obj);
    let reactor = builder
        .load(module, imports_obj)
        .await
        .map_err(anyhow::Error::new)?;

    Ok(WasiReactorInstance {
        reactor: Arc::new(reactor),
        stdin,
        stdout,
        stderr,
    })
}

/// Checks whether the WebAssembly memory is shared.
pub fn is_memory_shared(memory: &js_sys::WebAssembly::Memory) -> bool {
    memory
        .buffer()
        .is_instance_of::<js_sys::SharedArrayBuffer>()
}
