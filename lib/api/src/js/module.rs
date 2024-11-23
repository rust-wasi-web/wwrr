use crate::errors::RuntimeError;
use crate::imports::{Imports, ImportsObj};
use crate::js::AsJs;
use crate::store::AsStoreMut;
use crate::vm::VMInstance;
use crate::IntoBytes;
use crate::{errors::InstantiationError, js::js_handle::JsHandle};
use crate::{ExportType, ImportType};
use bytes::Bytes;
use js_sys::{Reflect, Uint8Array, WebAssembly};
use tracing::{debug, trace, warn};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use wasmer_types::{CompileError, ExportsIterator, ExternType, ImportsIterator, ModuleInfo};

/// WebAssembly in the browser doesn't yet output the descriptor/types
/// corresponding to each extern (import and export).
///
/// This should be fixed once the JS-Types Wasm proposal is adopted
/// by the browsers:
/// <https://github.com/WebAssembly/js-types/blob/master/proposals/js-types/Overview.md>
///
/// Until that happens, we annotate the module with the expected
/// types so we can built on top of them at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleTypeHints {
    /// The type hints for the imported types
    pub imports: Vec<ExternType>,
    /// The type hints for the exported types
    pub exports: Vec<ExternType>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Module {
    /// Module.
    module: JsHandle<WebAssembly::Module>,
    /// Name.
    name: Option<String>,
    /// WebAssembly type hints
    pub type_hints: ModuleTypeHints,
    /// Raw bytes.
    pub raw_bytes: Bytes,
}

// Module implements `structuredClone` in js, so it's safe it to make it Send.
// https://developer.mozilla.org/en-US/docs/Web/API/structuredClone
// ```js
// const module = new WebAssembly.Module(new Uint8Array([
//   0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00
// ]));
// structuredClone(module)
// ```
unsafe impl Send for Module {}
unsafe impl Sync for Module {}

impl From<Module> for JsValue {
    fn from(val: Module) -> Self {
        Self::from(val.module)
    }
}

impl Module {
    /// Creates a new WebAssembly module from its binary data.
    pub(crate) async fn from_binary(binary: &[u8]) -> Result<Self, CompileError> {
        let js_bytes = unsafe { Uint8Array::view(binary) };
        let module = JsFuture::from(WebAssembly::compile(&js_bytes))
            .await
            .map(|v| v.into())
            .map_err(|e| CompileError::Validate(format!("{}", e.as_string().unwrap())))?;

        Ok(Self::from_module_and_binary(module, binary))
    }

    /// Creates a new WebAssembly module from the compiled module and its binary data.
    pub(crate) fn from_module_and_binary(
        module: WebAssembly::Module,
        binary: impl IntoBytes,
    ) -> Self {
        let binary = binary.into_bytes();

        // The module is now validated, so we can safely parse it's types
        let info = crate::module_info_polyfill::translate_module(&binary[..])
            .expect("parsing module types failed");
        let type_hints = ModuleTypeHints {
            imports: info
                .info
                .imports()
                .map(|import| import.ty().clone())
                .collect::<Vec<_>>(),
            exports: info
                .info
                .exports()
                .map(|export| export.ty().clone())
                .collect::<Vec<_>>(),
        };

        Self {
            module: JsHandle::new(module),
            type_hints,
            name: info.info.name,
            raw_bytes: binary,
        }
    }

    pub fn validate(binary: &[u8]) -> Result<(), CompileError> {
        let js_bytes = unsafe { Uint8Array::view(binary) };
        match WebAssembly::validate(&js_bytes.into()) {
            Ok(true) => Ok(()),
            _ => Err(CompileError::Validate("Invalid Wasm file".to_owned())),
        }
    }

    pub(crate) fn instantiate(
        &self,
        store: &mut impl AsStoreMut,
        imports: &Imports,
        imports_obj: ImportsObj,
    ) -> Result<VMInstance, RuntimeError> {
        // Ensure all imports come from the same store.
        if imports
            .into_iter()
            .any(|(_, import)| !import.is_from_store(store))
        {
            return Err(RuntimeError::user(Box::new(
                InstantiationError::DifferentStores,
            )));
        }

        let imports_object = imports_obj.0;

        for import_type in self.imports() {
            let resolved_import = imports.get_export(import_type.module(), import_type.name());

            if let wasmer_types::ExternType::Memory(mem_ty) = import_type.ty() {
                if resolved_import.is_some() {
                    debug!("imported shared memory {:?}", &mem_ty);
                } else {
                    warn!(
                        "Error while importing {0:?}.{1:?}: memory. Expected {2:?}",
                        import_type.module(),
                        import_type.name(),
                        import_type.ty(),
                    );
                }
            }

            if let Some(import) = resolved_import {
                // Get or create the import namespace.
                let mut import_namespace =
                    js_sys::Reflect::get(&imports_object, &import_type.module().into())?;
                if import_namespace.is_undefined() {
                    import_namespace = js_sys::Object::new().into();
                    js_sys::Reflect::set(
                        &imports_object,
                        &import_type.module().into(),
                        &import_namespace.clone().into(),
                    )?;
                }

                // Set the import on the namespace.
                js_sys::Reflect::set(
                    &import_namespace,
                    &import_type.name().into(),
                    &import.as_jsvalue(&store.as_store_ref()),
                )?;

                trace!(
                    "resolved import {}:{} with internal function",
                    import_type.module(),
                    import_type.name()
                );
            } else {
                let import_namespace =
                    js_sys::Reflect::get(&imports_object, &import_type.module().into())?;
                let defined = !import_namespace.is_undefined()
                    && !js_sys::Reflect::get(&import_namespace, &import_type.name().into())?
                        .is_undefined();

                if defined {
                    trace!(
                        "resolved import {}:{} from provided imports object",
                        import_type.module(),
                        import_type.name()
                    );
                } else {
                    // in case the import is not found, the JS Wasm VM will handle
                    // the error for us, so we don't need to handle it
                    warn!(
                        "import {}:{} not found",
                        import_type.module(),
                        import_type.name()
                    );
                }
            }
        }

        Ok(WebAssembly::Instance::new(&self.module, &imports_object)
            .map_err(|e: JsValue| -> RuntimeError { e.into() })?)
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_ref().map(|s| s.as_ref())
    }

    pub fn serialize(&self) -> Bytes {
        self.raw_bytes.clone()
    }

    pub fn set_name(&mut self, name: &str) -> bool {
        self.name = Some(name.to_string());
        true
    }

    pub fn imports<'a>(&'a self) -> ImportsIterator<impl Iterator<Item = ImportType> + 'a> {
        let imports = WebAssembly::Module::imports(&self.module);
        let iter = imports
            .iter()
            .enumerate()
            .map(move |(i, val)| {
                let module = Reflect::get(val.as_ref(), &"module".into())
                    .unwrap()
                    .as_string()
                    .unwrap();
                let field = Reflect::get(val.as_ref(), &"name".into())
                    .unwrap()
                    .as_string()
                    .unwrap();
                let extern_type = self.type_hints.imports.get(i).unwrap().clone();
                ImportType::new(&module, &field, extern_type)
            })
            .collect::<Vec<_>>()
            .into_iter();
        ImportsIterator::new(iter, imports.length() as usize)
    }

    pub fn exports<'a>(&'a self) -> ExportsIterator<impl Iterator<Item = ExportType> + 'a> {
        let exports = WebAssembly::Module::exports(&self.module);
        let iter = exports
            .iter()
            .enumerate()
            .map(move |(i, val)| {
                let field = Reflect::get(val.as_ref(), &"name".into())
                    .unwrap()
                    .as_string()
                    .unwrap();
                let extern_type = self.type_hints.exports.get(i).unwrap().clone();
                ExportType::new(&field, extern_type)
            })
            .collect::<Vec<_>>()
            .into_iter();
        ExportsIterator::new(iter, exports.length() as usize)
    }

    pub fn custom_sections<'a>(&'a self, name: &'a str) -> impl Iterator<Item = Box<[u8]>> + 'a {
        WebAssembly::Module::custom_sections(&self.module, name)
            .iter()
            .map(move |buf_val| {
                let typebuf: js_sys::Uint8Array = js_sys::Uint8Array::new(&buf_val);
                typebuf.to_vec().into_boxed_slice()
            })
            .collect::<Vec<Box<[u8]>>>()
            .into_iter()
    }

    pub(crate) fn info(&self) -> &ModuleInfo {
        unimplemented!()
    }
}

impl From<crate::module::Module> for WebAssembly::Module {
    fn from(value: crate::module::Module) -> Self {
        value.0.module.into_inner()
    }
}
