use std::fmt;

use crate::exports::{Exports, ExportsObj};
use crate::imports::{Imports, ImportsObj};
use crate::js::instance as instance_imp;
use crate::module::Module;
use crate::store::AsStoreMut;
use crate::InstantiationError;

/// A WebAssembly Instance is a stateful, executable
/// instance of a WebAssembly [`Module`].
///
/// Instance objects contain all the exported WebAssembly
/// functions, memories, tables and globals that allow
/// interacting with WebAssembly.
///
/// Spec: <https://webassembly.github.io/spec/core/exec/runtime.html#module-instances>
#[derive(Clone, PartialEq, Eq)]
pub struct Instance {
    pub(crate) _inner: instance_imp::Instance,
    pub(crate) module: Module,
    /// The exports for an instance.
    pub exports: Exports,
    /// The raw JavaScript exports object of a WebAssembly instance.
    pub exports_obj: ExportsObj,
}

impl Instance {
    /// Creates a new `Instance` from a WebAssembly [`Module`] and a
    /// set of imports using [`Imports`] or the [`imports!`] macro helper.
    ///
    /// [`imports!`]: crate::imports!
    /// [`Imports!`]: crate::Imports!
    ///
    /// ```
    /// # use wasmer::{imports, Store, Module, Global, Value, Instance};
    /// # use wasmer::FunctionEnv;
    /// # fn main() -> anyhow::Result<()> {
    /// let mut store = Store::default();
    /// let env = FunctionEnv::new(&mut store, ());
    /// let module = Module::new(&store, "(module)")?;
    /// let imports = imports!{
    ///   "host" => {
    ///     "var" => Global::new(&mut store, Value::I32(2))
    ///   }
    /// };
    /// let instance = Instance::new(&mut store, &module, &imports)?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// ## Errors
    ///
    /// The function can return [`InstantiationError`]s.
    ///
    /// Those are, as defined by the spec:
    ///  * Link errors that happen when plugging the imports into the instance
    ///  * Runtime errors that happen when running the module `start` function.
    #[allow(clippy::result_large_err)]
    pub async fn new(
        store: &mut impl AsStoreMut,
        module: &Module,
        imports: &Imports,
        imports_obj: ImportsObj,
    ) -> Result<Self, InstantiationError> {
        let (_inner, exports, exports_obj) =
            instance_imp::Instance::new(store, module, imports, imports_obj).await?;
        Ok(Self {
            _inner,
            module: module.clone(),
            exports,
            exports_obj,
        })
    }

    /// Gets the [`Module`] associated with this instance.
    pub fn module(&self) -> &Module {
        &self.module
    }
}

impl fmt::Debug for Instance {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Instance")
            .field("exports", &self.exports)
            .finish()
    }
}
