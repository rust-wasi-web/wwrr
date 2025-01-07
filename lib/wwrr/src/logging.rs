use std::io::{ErrorKind, Write};

use tracing::Level;
use tracing_subscriber::{
    fmt::{format::FmtSpan, MakeWriter},
    EnvFilter,
};
use wasm_bindgen::{prelude::wasm_bindgen, JsValue};

/// Default log level.
const DEFAULT_LOG_LEVEL: &str = "warn";

#[wasm_bindgen(typescript_custom_section)]
const TYPE_DEFINITIONS: &'static str = r#"
/** Log configuration for the runtime. */
type LogConfig = {
    /**
     * The `filter` string can be used to tweak logging verbosity, both globally
     * or on a per-module basis, and follows [the `$RUST_LOG` format][format].
     *
     * Some examples:
     * - `off` - turn off all logs
     * - `error`, `warn`, `info`, `debug`, `trace` - set the global log level
     * - `wasmer_wasix` - enable logs for `wasmer_wasix`
     * - `info,wasmer_js::package_loader=trace` - set the global log level to
     *   `info` and set `wasmer_js::package_loader` to `trace`
     * - `wasmer_js=debug/flush` -  turns on debug logging for
     *   `wasmer_js` where the log message includes `flush`
     * - `warn,wasmer=info,wasmer_wasix::syscalls::wasi=trace` - directives can be
     *   mixed arbitrarily
     *
     * When no `filter` string is provided, a useful default will be used.
     *
     * [format]: https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives
     */
    filter?: string;
    /**
     * Whether to enable ANSI escape sequences for coloring the log output.
     */
    ansi?: boolean;
};
"#;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(typescript_type = "LogConfig", extends = js_sys::Object)]
    pub type LogConfig;

    #[wasm_bindgen(method, getter)]
    fn filter(this: &LogConfig) -> Option<String>;

    #[wasm_bindgen(method, getter)]
    fn ansi(this: &LogConfig) -> Option<bool>;
}

impl LogConfig {
    fn parse_filter(&self) -> String {
        self.filter()
            .unwrap_or_else(|| DEFAULT_LOG_LEVEL.to_string())
    }

    fn parse_ansi(&self) -> bool {
        self.ansi().unwrap_or_else(|| {
            let user_agent = web_sys::window()
                .and_then(|win| win.navigator().user_agent().ok())
                .unwrap_or_default();
            user_agent.contains("Chrome")
        })
    }
}

/// Initialize the logger used by the runtime.
///
/// This function can only be called once. Subsequent calls will raise an
/// exception.
#[wasm_bindgen(js_name = "initializeLogger")]
pub fn initialize_logger(log_config: LogConfig) -> Result<(), utils::Error> {
    let ansi = log_config.parse_ansi();

    let max_level = tracing::level_filters::STATIC_MAX_LEVEL
        .into_level()
        .unwrap_or(tracing::Level::ERROR);

    let filter = EnvFilter::builder()
        .with_regex(false)
        .with_default_directive(max_level.into())
        .parse_lossy(&log_config.parse_filter());

    tracing_subscriber::fmt::fmt()
        .with_writer(ConsoleLogger::spawn(ansi))
        .with_env_filter(filter)
        .with_span_events(FmtSpan::CLOSE)
        .without_time()
        .with_ansi(ansi)
        .try_init()
        .map_err(|e| anyhow::anyhow!(e))?;

    Ok(())
}

/// A [`std::io::Write`] implementation which will log all message using
/// [`web_sys::console`].
#[derive(Debug)]
struct ConsoleLogger {
    buffer: Vec<u8>,
    ansi: bool,
    level: Option<Level>,
}

impl ConsoleLogger {
    const PREFIX: &str = "WWRR";

    fn spawn(ansi: bool) -> impl for<'w> MakeWriter<'w> + 'static {
        struct MakeConsoleLogger {
            ansi: bool,
        }

        impl<'a> MakeWriter<'a> for MakeConsoleLogger {
            type Writer = ConsoleLogger;

            fn make_writer(&self) -> Self::Writer {
                ConsoleLogger {
                    buffer: Vec::new(),
                    ansi: self.ansi,
                    level: None,
                }
            }

            fn make_writer_for(&self, meta: &tracing::Metadata) -> Self::Writer {
                ConsoleLogger {
                    buffer: Vec::new(),
                    ansi: self.ansi,
                    level: Some(*meta.level()),
                }
            }
        }

        MakeConsoleLogger { ansi }
    }
}

impl Write for ConsoleLogger {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let buffer = std::mem::take(&mut self.buffer);
        let text = String::from_utf8(buffer)
            .map_err(|e| std::io::Error::new(ErrorKind::InvalidInput, e))?;

        let prefix = if self.ansi {
            format!("\x1b[35m{}\x1b[0m", Self::PREFIX)
        } else {
            Self::PREFIX.to_string()
        };

        let msg = format!("{prefix} {text}");
        let js_string = JsValue::from(msg);

        match self.level {
            Some(Level::DEBUG) => web_sys::console::debug_1(&js_string),
            Some(Level::INFO) => web_sys::console::info_1(&js_string),
            Some(Level::ERROR) => web_sys::console::error_1(&js_string),
            Some(Level::WARN) => web_sys::console::warn_1(&js_string),
            Some(Level::TRACE) => web_sys::console::trace_1(&js_string),
            None => web_sys::console::log_1(&js_string),
        }

        Ok(())
    }
}

impl Drop for ConsoleLogger {
    fn drop(&mut self) {
        if !self.buffer.is_empty() {
            let _ = self.flush();
        }
    }
}
