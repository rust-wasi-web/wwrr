use std::sync::LazyLock;

use js_sys::{Array, JsString, Uint8Array};
use wasm_bindgen::prelude::*;

/// The URL used by the bootstrapping script to import the `wasm-bindgen` glue
/// code.
pub fn import_meta_url() -> String {
    #[wasm_bindgen]
    #[allow(non_snake_case)]
    extern "C" {
        #[wasm_bindgen(thread_local_v2, js_namespace = ["import", "meta"], js_name = url)]
        static IMPORT_META_URL: String;
    }

    let import_meta_url = IMPORT_META_URL.with(|s| s.clone());

    let import_url = crate::CUSTOM_WORKER_URL.lock().unwrap();
    let import_url = import_url.as_deref().unwrap_or(&import_meta_url);

    import_url.to_string()
}

/// A data URL containing our worker's bootstrap script.
pub static WORKER_URL: LazyLock<String> = LazyLock::new(|| {
    let script = include_str!("worker.js");

    let bpb = web_sys::BlobPropertyBag::new();
    bpb.set_type("application/javascript");

    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(
        Array::from_iter([Uint8Array::from(script.as_bytes())]).as_ref(),
        &bpb,
    )
    .unwrap();

    web_sys::Url::create_object_url_with_blob(&blob).unwrap()
});

/// Craft the special `"init"` message for a worker.
pub fn init_message_worker(id: u32) -> JsValue {
    let msg = js_sys::Object::new();

    js_sys::Reflect::set(&msg, &JsString::from("type"), &JsString::from("init")).unwrap();
    js_sys::Reflect::set(&msg, &JsString::from("role"), &JsString::from("worker")).unwrap();
    js_sys::Reflect::set(&msg, &JsString::from("memory"), &wasm_bindgen::memory()).unwrap();
    js_sys::Reflect::set(&msg, &JsString::from("id"), &JsValue::from(id)).unwrap();
    js_sys::Reflect::set(
        &msg,
        &JsString::from("import_url"),
        &JsValue::from(import_meta_url()),
    )
    .unwrap();
    js_sys::Reflect::set(&msg, &JsString::from("module"), &utils::current_module()).unwrap();

    msg.into()
}

/// Craft the special `"init"` message for the scheduler.
pub fn init_message_scheduler() -> JsValue {
    let msg = js_sys::Object::new();

    js_sys::Reflect::set(&msg, &JsString::from("type"), &JsString::from("init")).unwrap();
    js_sys::Reflect::set(&msg, &JsString::from("role"), &JsString::from("scheduler")).unwrap();
    js_sys::Reflect::set(&msg, &JsString::from("memory"), &wasm_bindgen::memory()).unwrap();
    js_sys::Reflect::set(
        &msg,
        &JsString::from("import_url"),
        &JsValue::from(import_meta_url()),
    )
    .unwrap();
    js_sys::Reflect::set(&msg, &JsString::from("module"), &utils::current_module()).unwrap();

    msg.into()
}
