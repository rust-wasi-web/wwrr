use bytes::Bytes;
use js_sys::WebAssembly;
use std::fmt;
use std::io;
use std::sync::Arc;

use thiserror::Error;
#[cfg(feature = "wat")]
use wasmer_types::WasmError;
use wasmer_types::{CompileError, ExportsIterator, ImportsIterator, ModuleInfo};
use wasmer_types::{ExportType, ImportType};

use crate::into_bytes::IntoBytes;
use crate::js::module as module_imp;
use crate::ModuleTypeHints;

/// IO Error on a Module Compilation
#[derive(Error, Debug)]
pub enum IoCompileError {
    /// An IO error
    #[error(transparent)]
    Io(#[from] io::Error),
    /// A compilation error
    #[error(transparent)]
    Compile(#[from] CompileError),
}

/// A WebAssembly Module contains stateless WebAssembly
/// code that has already been compiled and can be instantiated
/// multiple times.
///
/// ## Cloning a module
///
/// Cloning a module is cheap: it does a shallow copy of the compiled
/// contents rather than a deep copy.
#[derive(Clone, PartialEq, Eq)]
pub struct Module(pub(crate) module_imp::Module);

impl Module {
    /// Creates a new WebAssembly Module given the configuration
    /// in the store.
    ///
    /// If the provided bytes are not WebAssembly-like (start with `b"\0asm"`),
    /// and the "wat" feature is enabled for this crate, this function will try to
    /// to convert the bytes assuming they correspond to the WebAssembly text
    /// format.
    ///
    /// ## Security
    ///
    /// Before the code is compiled, it will be validated using the store
    /// features.
    ///
    /// ## Errors
    ///
    /// Creating a WebAssembly module from bytecode can result in a
    /// [`CompileError`] since this operation requires to transorm the Wasm
    /// bytecode into code the machine can easily execute.
    #[cfg(feature = "wat")]
    pub async fn from_wat(bytes: impl AsRef<[u8]>) -> Result<Self, CompileError> {
        let bytes = wat::parse_bytes(bytes.as_ref()).map_err(|e| {
            CompileError::Wasm(WasmError::Generic(format!(
                "Error when converting wat: {}",
                e
            )))
        })?;

        let bytes = Bytes::from(bytes.to_vec());
        Self::from_binary(bytes).await
    }

    /// Creates a new WebAssembly module from a Wasm binary.
    ///
    /// Opposed to [`Module::new`], this function is not compatible with
    /// the WebAssembly text format (if the "wat" feature is enabled for
    /// this crate).
    pub async fn from_binary(binary: Bytes) -> Result<Self, CompileError> {
        Ok(Self(module_imp::Module::from_binary(binary).await?))
    }

    /// Creates a new WebAssembly module from the compiled module and its binary data.
    pub fn from_module_and_binary(module: WebAssembly::Module, binary: Bytes) -> Self {
        Self(module_imp::Module::from_module_and_binary(
            module,
            binary.into_bytes(),
        ))
    }

    /// Creates a new WebAssembly module from the compiled module and its name and type hints.
    pub fn from_module_name_and_type_hints(
        module: WebAssembly::Module,
        name: Option<String>,
        type_hints: Arc<ModuleTypeHints>,
    ) -> Self {
        Self(module_imp::Module::from_module_name_and_type_hints(
            module, name, type_hints,
        ))
    }

    /// Validates a new WebAssembly Module given the configuration
    /// in the Store.
    ///
    /// This validation is normally pretty fast and checks the enabled
    /// WebAssembly features in the Store Engine to assure deterministic
    /// validation of the Module.
    pub fn validate(binary: &[u8]) -> Result<(), CompileError> {
        module_imp::Module::validate(binary)
    }

    /// Serializes a module into a binary representation that the `Engine`
    /// can later process via [`Module::deserialize`].
    ///
    /// # Important
    ///
    /// This function will return a custom binary format that will be different than
    /// the `wasm` binary format, but faster to load in Native hosts.
    ///
    /// # Usage
    ///
    /// ```ignore
    /// # use wasmer::*;
    /// # fn main() -> anyhow::Result<()> {
    /// # let mut store = Store::default();
    /// # let module = Module::from_file(&store, "path/to/foo.wasm")?;
    /// let serialized = module.serialize()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn type_hints(&self) -> Arc<ModuleTypeHints> {
        self.0.type_hints()
    }

    /// Returns the name of the current module.
    ///
    /// This name is normally set in the WebAssembly bytecode by some
    /// compilers, but can be also overwritten using the [`Module::set_name`] method.
    ///
    /// # Example
    ///
    /// ```
    /// # use wasmer::*;
    /// # fn main() -> anyhow::Result<()> {
    /// # let mut store = Store::default();
    /// let wat = "(module $moduleName)";
    /// let module = Module::new(&store, wat)?;
    /// assert_eq!(module.name(), Some("moduleName"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn name(&self) -> Option<&str> {
        self.0.name()
    }

    /// Sets the name of the current module.
    /// This is normally useful for stacktraces and debugging.
    ///
    /// It will return `true` if the module name was changed successfully,
    /// and return `false` otherwise (in case the module is cloned or
    /// already instantiated).
    ///
    /// # Example
    ///
    /// ```
    /// # use wasmer::*;
    /// # fn main() -> anyhow::Result<()> {
    /// # let mut store = Store::default();
    /// let wat = "(module)";
    /// let mut module = Module::new(&store, wat)?;
    /// assert_eq!(module.name(), None);
    /// module.set_name("foo");
    /// assert_eq!(module.name(), Some("foo"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_name(&mut self, name: &str) -> bool {
        self.0.set_name(name)
    }

    /// Returns an iterator over the imported types in the Module.
    ///
    /// The order of the imports is guaranteed to be the same as in the
    /// WebAssembly bytecode.
    ///
    /// # Example
    ///
    /// ```
    /// # use wasmer::*;
    /// # fn main() -> anyhow::Result<()> {
    /// # let mut store = Store::default();
    /// let wat = r#"(module
    ///     (import "host" "func1" (func))
    ///     (import "host" "func2" (func))
    /// )"#;
    /// let module = Module::new(&store, wat)?;
    /// for import in module.imports() {
    ///     assert_eq!(import.module(), "host");
    ///     assert!(import.name().contains("func"));
    ///     import.ty();
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn imports(&self) -> ImportsIterator<impl Iterator<Item = ImportType> + '_> {
        self.0.imports()
    }

    /// Returns an iterator over the exported types in the Module.
    ///
    /// The order of the exports is guaranteed to be the same as in the
    /// WebAssembly bytecode.
    ///
    /// # Example
    ///
    /// ```
    /// # use wasmer::*;
    /// # fn main() -> anyhow::Result<()> {
    /// # let mut store = Store::default();
    /// let wat = r#"(module
    ///     (func (export "namedfunc"))
    ///     (memory (export "namedmemory") 1)
    /// )"#;
    /// let module = Module::new(&store, wat)?;
    /// for export_ in module.exports() {
    ///     assert!(export_.name().contains("named"));
    ///     export_.ty();
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn exports(&self) -> ExportsIterator<impl Iterator<Item = ExportType> + '_> {
        self.0.exports()
    }

    /// Get the custom sections of the module given a `name`.
    ///
    /// # Important
    ///
    /// Following the WebAssembly spec, one name can have multiple
    /// custom sections. That's why an iterator (rather than one element)
    /// is returned.
    pub fn custom_sections<'a>(&'a self, name: &'a str) -> impl Iterator<Item = Box<[u8]>> + 'a {
        self.0.custom_sections(name)
    }

    /// The ABI of the [`ModuleInfo`] is very unstable, we refactor it very often.
    /// This function is public because in some cases it can be useful to get some
    /// extra information from the module.
    ///
    /// However, the usage is highly discouraged.
    #[doc(hidden)]
    pub fn info(&self) -> &ModuleInfo {
        self.0.info()
    }
}

impl fmt::Debug for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Module")
            .field("name", &self.name())
            .finish()
    }
}

impl From<Module> for wasm_bindgen::JsValue {
    fn from(value: Module) -> Self {
        wasm_bindgen::JsValue::from(value.0)
    }
}
