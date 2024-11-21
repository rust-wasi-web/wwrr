use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
};

use wasm_bindgen::prelude::wasm_bindgen;

use crate::{runtime::Runtime, utils::Error};

#[derive(Clone, Debug)]
#[repr(transparent)]
#[wasm_bindgen(js_name = "Runtime")]
pub struct JsRuntime {
    // Note: This is just a wrapper around the "real" runtime implementation.
    // We need it because most JS-facing methods will want to accept/return an
    // `Arc<Runtime>`, but wasm-bindgen doesn't support passing Arc around
    // directly.
    rt: Arc<Runtime>,
}

impl JsRuntime {
    pub fn new(rt: Arc<Runtime>) -> Self {
        JsRuntime { rt }
    }

    pub fn into_inner(self) -> Arc<Runtime> {
        self.rt
    }
}

#[wasm_bindgen(js_class = "Runtime")]
impl JsRuntime {
    #[wasm_bindgen(constructor)]
    pub fn js_new() -> Result<JsRuntime, Error> {
        let rt = Runtime::new();
        Ok(JsRuntime::new(Arc::new(rt)))
    }

    /// Get a reference to the global runtime, optionally initializing it if
    /// requested.
    pub fn global(initialize: Option<bool>) -> Result<Option<JsRuntime>, Error> {
        match Runtime::global() {
            Some(rt) => Ok(Some(JsRuntime { rt })),
            None if initialize == Some(true) => {
                let rt = Runtime::lazily_initialized()?;
                Ok(Some(JsRuntime { rt }))
            }
            None => Ok(None),
        }
    }
}

impl Deref for JsRuntime {
    type Target = Arc<Runtime>;

    fn deref(&self) -> &Self::Target {
        &self.rt
    }
}

impl DerefMut for JsRuntime {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.rt
    }
}

impl From<Arc<Runtime>> for JsRuntime {
    fn from(value: Arc<Runtime>) -> Self {
        JsRuntime::new(value)
    }
}

impl AsRef<Arc<Runtime>> for JsRuntime {
    fn as_ref(&self) -> &Arc<Runtime> {
        self
    }
}
