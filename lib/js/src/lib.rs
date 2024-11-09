// #![feature(once_cell_try)]

#[cfg(test)]
wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

pub mod fs;
mod instance;
mod js_runtime;
mod logging;
mod options;
mod package_loader;
mod run;
mod runtime;
mod streams;
mod tasks;
mod utils;

pub use crate::{
    fs::{Directory, DirectoryInit},
    instance::{Instance, JsOutput},
    js_runtime::JsRuntime,
    logging::initialize_logger,
    options::{RunOptions, SpawnOptions},
    run::run_wasix,
    utils::StringOrBytes,
};

use once_cell::sync::Lazy;
use std::sync::Mutex;
use wasm_bindgen::prelude::wasm_bindgen;

pub(crate) const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
pub(crate) const DEFAULT_RUST_LOG: &[&str] = &["warn"];
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
