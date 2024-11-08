mod as_js;
pub(crate) mod engine;
pub(crate) mod errors;
pub(crate) mod extern_ref;
pub(crate) mod externals;
pub(crate) mod instance;
mod js_handle;
pub(crate) mod mem_access;
pub(crate) mod module;
pub(crate) mod store;
pub(crate) mod trap;
pub(crate) mod typed_function;
pub(crate) mod vm;
mod wasm_bindgen_polyfill;

pub use self::{as_js::AsJs, js_handle::current_thread_id, module::ModuleTypeHints};
