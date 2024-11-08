mod client;
mod web_http_client;

pub use self::web_http_client::WebHttpClient;
pub use self::client::*;

pub(crate) const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "-", env!("CARGO_PKG_VERSION"));

/// Try to instantiate a HTTP client that is suitable for the current platform.
pub fn default_http_client() -> Option<impl HttpClient + Send + Sync + 'static> {
    Some(web_http_client::WebHttpClient::default())
}
