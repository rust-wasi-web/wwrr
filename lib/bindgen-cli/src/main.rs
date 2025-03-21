use anyhow::{bail, Error};
use docopt::Docopt;
use serde::Deserialize;
use std::path::PathBuf;
use std::process;
use wasm_bindgen_cli_support::{Bindgen, EncodeInto};

const USAGE: &str = "
Generating JS bindings for WWRR (internal)

Usage:
    bindgen-cli [options] <input>
    bindgen-cli -h | --help
    bindgen-cli -V | --version

Options:
    -h --help                    Show this screen.
    --out-dir DIR                Output directory
    --out-name VAR               Set a custom output filename (Without extension. Defaults to crate name)
    --target TARGET              What type of output to generate, valid
                                 values are [web, bundler, nodejs, no-modules, deno, experimental-nodejs-module],
                                 and the default is [bundler]
    --no-modules-global VAR      Name of the global variable to initialize
    --browser                    Hint that JS should only be compatible with a browser
    --typescript                 Output a TypeScript definition file (on by default)
    --no-typescript              Don't emit a *.d.ts file
    --omit-imports               Don't emit imports in generated JavaScript
    --debug                      Include otherwise-extraneous debug checks in output
    --no-demangle                Don't demangle Rust symbol names
    --keep-lld-exports           Keep exports synthesized by LLD
    --keep-debug                 Keep debug sections in Wasm files
    --remove-name-section        Remove the debugging `name` section of the file
    --remove-producers-section   Remove the telemetry `producers` section
    --omit-default-module-path   Don't add WebAssembly fallback imports in generated JavaScript
    --split-linked-modules       Split linked modules out into their own files. Recommended if possible.
                                 If a bundler is used, it needs to be set up accordingly.
    --encode-into MODE           Whether or not to use TextEncoder#encodeInto,
                                 valid values are [test, always, never]
    -V --version                 Print the version number of wasm-bindgen
";

#[derive(Debug, Deserialize)]
struct Args {
    flag_browser: bool,
    flag_typescript: bool,
    flag_no_typescript: bool,
    flag_omit_imports: bool,
    flag_out_dir: Option<PathBuf>,
    flag_out_name: Option<String>,
    flag_debug: bool,
    flag_version: bool,
    flag_no_demangle: bool,
    flag_no_modules_global: Option<String>,
    flag_remove_name_section: bool,
    flag_remove_producers_section: bool,
    #[allow(dead_code)]
    flag_keep_lld_exports: bool,
    flag_keep_debug: bool,
    flag_encode_into: Option<String>,
    flag_target: Option<String>,
    flag_omit_default_module_path: bool,
    flag_split_linked_modules: bool,
    arg_input: Option<PathBuf>,
}

fn main() {
    tracing_subscriber::fmt::init();

    let args: Args = Docopt::new(USAGE)
        .and_then(|d| d.deserialize())
        .unwrap_or_else(|e| e.exit());

    if args.flag_version {
        println!(
            "bindgen-cli using wasm-bindgen {}",
            wasm_bindgen_shared::version()
        );
        return;
    }
    let err = match rmain(&args) {
        Ok(()) => return,
        Err(e) => e,
    };
    eprintln!("error: {:?}", err);
    process::exit(1);
}

fn rmain(args: &Args) -> Result<(), Error> {
    let input = match args.arg_input {
        Some(ref s) => s,
        None => bail!("input file expected"),
    };

    let typescript = args.flag_typescript || !args.flag_no_typescript;

    let mut b = Bindgen::new();
    if let Some(name) = &args.flag_target {
        match name.as_str() {
            "bundler" => b.bundler(true)?,
            "web" => b.web(true)?,
            "no-modules" => b.no_modules(true)?,
            "nodejs" => b.nodejs(true)?,
            "deno" => b.deno(true)?,
            "experimental-nodejs-module" => b.nodejs_module(true)?,
            s => bail!("invalid encode-into mode: `{}`", s),
        };
    }
    b.input_path(input)
        .browser(args.flag_browser)?
        .debug(args.flag_debug)
        .demangle(!args.flag_no_demangle)
        .keep_lld_exports(args.flag_keep_lld_exports)
        .keep_debug(args.flag_keep_debug)
        .remove_name_section(args.flag_remove_name_section)
        .remove_producers_section(args.flag_remove_producers_section)
        .typescript(typescript)
        .omit_imports(args.flag_omit_imports)
        .omit_default_module_path(args.flag_omit_default_module_path)
        .split_linked_modules(args.flag_split_linked_modules)
        .wait(true);
    if let Some(ref name) = args.flag_no_modules_global {
        b.no_modules_global(name)?;
    }
    if let Some(ref name) = args.flag_out_name {
        b.out_name(name);
    }
    if let Some(mode) = &args.flag_encode_into {
        match mode.as_str() {
            "test" => b.encode_into(EncodeInto::Test),
            "always" => b.encode_into(EncodeInto::Always),
            "never" => b.encode_into(EncodeInto::Never),
            s => bail!("invalid encode-into mode: `{}`", s),
        };
    }

    let out_dir = match args.flag_out_dir {
        Some(ref p) => p,
        None => bail!("the `--out-dir` argument is now required"),
    };

    b.generate(out_dir)
}
