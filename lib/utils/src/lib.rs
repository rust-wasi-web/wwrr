use std::cell::LazyCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    collections::BTreeMap,
    fmt::{Debug, Display},
    num::NonZeroUsize,
};

use js_sys::{JsString, Promise, Uint8Array};

use wasm_bindgen::{prelude::wasm_bindgen, JsCast, JsValue};
use web_sys::{Window, WorkerGlobalScope};

/// Try to extract the most appropriate error message from a [`JsValue`],
/// falling back to a generic error message.
pub fn js_error(value: JsValue) -> anyhow::Error {
    if let Some(e) = value.dyn_ref::<js_sys::Error>() {
        anyhow::Error::msg(String::from(e.message()))
    } else if let Some(obj) = value.dyn_ref::<js_sys::Object>() {
        return anyhow::Error::msg(String::from(obj.to_string()));
    } else if let Some(s) = value.dyn_ref::<js_sys::JsString>() {
        return anyhow::Error::msg(String::from(s));
    } else {
        anyhow::anyhow!("An unknown error occurred: {value:?}")
    }
}

/// A strongly-typed wrapper around `globalThis`.
#[derive(Debug, Clone, PartialEq)]
pub enum GlobalScope {
    Window(Window),
    Worker(WorkerGlobalScope),
}

impl GlobalScope {
    /// Current global scope.
    pub fn current() -> Self {
        thread_local! {
            static INSTANCE: LazyCell<GlobalScope> = LazyCell::new(|| {
                match js_sys::global().dyn_into() {
                    Ok(window) => GlobalScope::Window(window),
                    Err(global_scope) => match global_scope.dyn_into() {
                        Ok(worker_global_scope) => GlobalScope::Worker(worker_global_scope),
                        Err(other) => panic!("unrecognized global scope {other:?}"),
                    },
                }
            });
        }

        INSTANCE.with(|instance| (*instance).clone())
    }

    /// Whether waiting is allowed.
    pub fn is_wait_allowed(&self) -> bool {
        match self {
            GlobalScope::Window(_) => false,
            GlobalScope::Worker(_) => true,
        }
    }

    /// Sleep for given duration.
    pub fn sleep(&self, milliseconds: i32) -> Promise {
        Promise::new(&mut |resolve, _reject| match self {
            GlobalScope::Window(window) => {
                window
                    .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, milliseconds)
                    .unwrap();
            }
            GlobalScope::Worker(worker_global_scope) => {
                worker_global_scope
                    .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, milliseconds)
                    .unwrap();
            }
        })
    }

    /// Gets the current time in milliseconds.
    pub fn now(&self) -> f64 {
        let now = match self {
            GlobalScope::Window(window) => {
                let performance = window.performance().unwrap();
                performance.now() + performance.time_origin()
            }
            GlobalScope::Worker(worker) => {
                let performance = worker.performance().unwrap();
                performance.now() + performance.time_origin()
            }
        };

        static START: AtomicU64 = AtomicU64::new(0);
        let start =
            match START.compare_exchange(0, now as u64, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => now as u64,
                Err(start) => start,
            };
        now - start as f64
    }

    /// The amount of concurrency available on this system.
    ///
    /// Returns `None` if unable to determine the available concurrency.
    pub fn hardware_concurrency(&self) -> Option<NonZeroUsize> {
        let concurrency = match self {
            GlobalScope::Window(scope) => scope.navigator().hardware_concurrency(),
            GlobalScope::Worker(scope) => scope.navigator().hardware_concurrency(),
        };

        let concurrency = concurrency.round() as usize;
        NonZeroUsize::new(concurrency)
    }

    /// Whether the current site is cross-origin isolated.
    pub fn cross_origin_isolated(&self) -> Option<bool> {
        let obj = self.as_object();
        js_sys::Reflect::get(obj, &JsValue::from_str("crossOriginIsolated"))
            .ok()
            .and_then(|obj| obj.as_bool())
    }

    /// Browser information.
    pub fn navigator(&self) -> NavigatorInfo {
        let user_agent = match self {
            Self::Window(scope) => scope.navigator().user_agent().unwrap(),
            Self::Worker(scope) => scope.navigator().user_agent().unwrap(),
        };

        NavigatorInfo(user_agent)
    }

    fn as_object(&self) -> &js_sys::Object {
        match self {
            GlobalScope::Window(w) => w,
            GlobalScope::Worker(w) => w,
        }
    }
}

/// Browser information.
#[derive(Debug, Clone)]
pub struct NavigatorInfo(String);

impl NavigatorInfo {
    fn is_chrome_like(&self) -> bool {
        self.0.to_lowercase().contains("chrome")
    }

    fn is_firefox_like(&self) -> bool {
        let ua = self.0.to_lowercase();
        ua.contains("firefox") || ua.contains("gecko/")
    }

    /// Whether the browser has event loops that are independent between workers.
    ///
    /// Currently this is only the case for Chrome and Edge.
    ///
    /// When the browser has no independent event loops and the UI thread is blocked by
    /// running code, some events like timeouts will not fire on web workers.
    pub fn has_independent_event_loops(&self) -> bool {
        self.is_chrome_like()
    }

    /// Whether the browser requires a Uint8Array in *browser* memory for WebAssembly compilation.
    ///
    /// Currently this is the case for Firefox.
    pub fn requires_browser_compile_buffer(&self) -> bool {
        self.is_firefox_like()
    }
}

/// A wrapper around [`anyhow::Error`] that can be returned to JS to raise
/// an exception.
#[derive(Debug)]
pub enum Error {
    Rust(anyhow::Error),
    JavaScript(JsValue),
}

impl Error {
    pub fn js(error: impl Into<JsValue>) -> Self {
        Error::JavaScript(error.into())
    }

    pub fn into_anyhow(self) -> anyhow::Error {
        match self {
            Error::Rust(e) => e,
            Error::JavaScript(js) => js_error(js),
        }
    }
}

impl<E: Into<anyhow::Error>> From<E> for Error {
    fn from(value: E) -> Self {
        Error::Rust(value.into())
    }
}

impl From<Error> for JsValue {
    fn from(error: Error) -> Self {
        match error {
            Error::JavaScript(e) => e,
            Error::Rust(error) => {
                let message = error.to_string();
                let js_error = js_sys::Error::new(&message);

                let _ = js_sys::Reflect::set(
                    &js_error,
                    &JsString::from("message"),
                    &JsString::from(error.to_string()),
                );

                let _ = js_sys::Reflect::set(
                    &js_error,
                    &JsString::from("detailedMessage"),
                    &JsString::from(format!("{error:?}")),
                );

                let causes: js_sys::Array = std::iter::successors(error.source(), |e| e.source())
                    .map(|e| JsString::from(e.to_string()))
                    .collect();
                let _ = js_sys::Reflect::set(&js_error, &JsString::from("causes"), &causes);

                js_error.into()
            }
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Rust(e) => Display::fmt(e, f),
            Error::JavaScript(js) => {
                if let Some(e) = js.dyn_ref::<js_sys::Error>() {
                    write!(f, "{}", String::from(e.message()))
                } else if let Some(obj) = js.dyn_ref::<js_sys::Object>() {
                    write!(f, "{}", String::from(obj.to_string()))
                } else if let Some(s) = js.dyn_ref::<js_sys::JsString>() {
                    write!(f, "{}", String::from(s))
                } else {
                    write!(f, "A JavaScript error occurred")
                }
            }
        }
    }
}

pub fn object_entries(obj: &js_sys::Object) -> Result<BTreeMap<JsString, JsValue>, Error> {
    let mut entries = BTreeMap::new();

    for key in js_sys::Object::keys(obj) {
        let key: JsString = key
            .dyn_into()
            .map_err(|_| Error::js(js_sys::TypeError::new("Object keys should be strings")))?;
        let value = js_sys::Reflect::get(obj, &key).map_err(Error::js)?;
        entries.insert(key, value);
    }

    Ok(entries)
}

/// A dummy value that can be used in a [`Debug`] impl instead of showing the
/// original value.
pub fn hidden<T>(_value: T, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("_")
}

/// Get a reference to the currently running module.
pub fn current_module() -> js_sys::WebAssembly::Module {
    // FIXME: Switch this to something stable and portable
    //
    // We use an undocumented API to get a reference to the
    // WebAssembly module that is being executed right now so start
    // a new thread by transferring the WebAssembly linear memory and
    // module to a worker and beginning execution.
    //
    // This can only be used in the browser. Trying to build
    // wasmer-wasix for NodeJS will probably result in the following:
    //
    // Error: executing `wasm-bindgen` over the wasm file
    //   Caused by:
    //   0: failed to generate bindings for import of `__wbindgen_placeholder__::__wbindgen_module`
    //   1: `wasm_bindgen::module` is currently only supported with `--target no-modules` and `--tar get web`
    wasm_bindgen::module().dyn_into().unwrap()
}

pub fn js_string_array(array: js_sys::Array) -> Result<Vec<String>, Error> {
    let mut parsed = Vec::new();

    for arg in array {
        match arg.dyn_into::<JsString>() {
            Ok(arg) => parsed.push(String::from(arg)),
            Err(_) => {
                return Err(Error::js(js_sys::TypeError::new(
                    "Expected an array of strings",
                )));
            }
        }
    }

    Ok(parsed)
}

pub fn js_record_of_strings(obj: &js_sys::Object) -> Result<Vec<(String, String)>, Error> {
    let mut parsed = Vec::new();

    for (key, value) in object_entries(obj)? {
        let key: String = key.into();
        let value: String = value
            .dyn_into::<JsString>()
            .map_err(|_| {
                Error::js(js_sys::TypeError::new(
                    "Expected an object mapping strings to strings",
                ))
            })?
            .into();
        parsed.push((key, value));
    }

    Ok(parsed)
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(typescript_type = "string | Uint8Array")]
    pub type StringOrBytes;
}

impl StringOrBytes {
    pub fn as_bytes(&self) -> Vec<u8> {
        if let Some(s) = self.dyn_ref::<JsString>() {
            String::from(s).into()
        } else if let Some(buffer) = self.dyn_ref::<Uint8Array>() {
            buffer.to_vec()
        } else {
            unreachable!()
        }
    }
}
