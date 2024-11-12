mod client;
mod web_http_client;

pub use self::client::*;
pub use self::web_http_client::WebHttpClient;

/// Try to instantiate a HTTP client that is suitable for the current platform.
pub fn default_http_client() -> Option<impl HttpClient + Send + Sync + 'static> {
    Some(web_http_client::WebHttpClient::default())
}
