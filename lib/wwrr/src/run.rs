use futures::channel::oneshot;
use futures::FutureExt;
use std::sync::Arc;
use wasm_bindgen::{prelude::*, JsCast};
use wasmer::ImportsObj;
use wasmer_wasix::{Runtime as _, WasiEnvBuilder, WasiReactor};

use crate::{instance::ExitCondition, utils::Error, Instance, RunOptions};

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
        if let Some(buffer) = self.dyn_ref::<js_sys::Uint8Array>() {
            let buffer = buffer.to_vec();
            let module = runtime.load_module(&buffer).await?;
            Ok(module)
        } else {
            unreachable!();
        }
    }
}

/// Run a WASIX program.
///
/// # WASI Compatibility
///
/// The WASIX standard is a superset of [WASI preview 1][preview-1], so programs
/// compiled to WASI will run without any problems.
///
/// [WASI Preview 2][preview-2] is a backwards incompatible rewrite of WASI
/// to use the experimental [Component Model Proposal][component-model]. That
/// means programs compiled for WASI Preview 2 will fail to load.
///
/// [preview-1]: https://github.com/WebAssembly/WASI/blob/main/legacy/README.md
/// [preview-2]: https://github.com/WebAssembly/WASI/blob/main/preview2/README.md
/// [component-model]: https://github.com/WebAssembly/component-model
#[wasm_bindgen(js_name = "runWasix")]
pub async fn run_wasix(wasm_module: WasmModule, config: RunOptions) -> Result<Instance, Error> {
    run_wasix_inner(wasm_module, config).await
}

#[tracing::instrument(level = "debug", skip_all)]
async fn run_wasix_inner(wasm_module: WasmModule, config: RunOptions) -> Result<Instance, Error> {
    let mut runtime = config.runtime().resolve()?.into_inner();
    // We set it up with the default pool
    runtime = Arc::new(runtime.with_default_pool());

    let program_name = config
        .program()
        .as_string()
        .unwrap_or_else(|| DEFAULT_PROGRAM_NAME.to_string());

    let mut builder = WasiEnvBuilder::new(program_name).runtime(runtime.clone());
    let (stdin, stdout, stderr) = config.configure_builder(&mut builder)?;

    let (exit_code_tx, exit_code_rx) = oneshot::channel();

    let module: wasmer::Module = wasm_module.to_module(&*runtime).await?;

    // Note: The WasiEnvBuilder::run() method blocks, so we need to run it on
    // the thread pool.
    let tasks = runtime.task_manager().clone();
    tasks.spawn_with_module(
        module,
        Box::new(move |module| {
            async move {
                let _span = tracing::debug_span!("run").entered();
                let result = builder.run(module).await.map_err(anyhow::Error::new);
                let _ = exit_code_tx.send(ExitCondition::from_result(result));
            }
            .boxed_local()
        }),
    )?;

    Ok(Instance {
        stdin,
        stdout,
        stderr,
        exit: exit_code_rx,
    })
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
    assert!(
        is_memory_shared(our_memory.dyn_ref().unwrap()),
        "wwrr memory is not shared"
    );

    let mut runtime = config.runtime().resolve()?.into_inner();
    runtime = Arc::new(runtime.with_default_pool());

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

    tracing::info!("module loaded!");

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
