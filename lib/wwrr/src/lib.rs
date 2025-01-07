#[cfg(test)]
wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

pub mod fs;
mod js_runtime;
mod logging;
mod options;
mod run;
mod runtime;
mod streams;
mod tasks;

pub use crate::{
    fs::{Directory, DirectoryInit},
    js_runtime::JsRuntime,
    logging::initialize_logger,
    options::{RunOptions, SpawnOptions},
};
pub use utils::StringOrBytes;

use once_cell::sync::Lazy;
use std::sync::Mutex;
use wasm_bindgen::prelude::wasm_bindgen;

pub(crate) static CUSTOM_WORKER_URL: Lazy<Mutex<Option<String>>> = Lazy::new(Mutex::default);

#[wasm_bindgen]
pub fn wat2wasm(wat: String) -> Result<js_sys::Uint8Array, utils::Error> {
    let wasm = ::wasmer::wat2wasm(wat.as_bytes())?;
    Ok(wasm.as_ref().into())
}

#[wasm_bindgen(start, skip_typescript)]
fn on_start() {
    std::panic::set_hook(Box::new(|p| {
        tracing::error!("{p}");
        console_error_panic_hook::hook(p);
    }));
}

#[wasm_bindgen(js_name = setWorkerUrl)]
pub fn set_worker_url(url: js_sys::JsString) {
    *CUSTOM_WORKER_URL.lock().unwrap() = Some(url.into());
}
