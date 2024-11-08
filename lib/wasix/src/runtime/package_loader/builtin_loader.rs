use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use anyhow::{Context, Error};
use bytes::Bytes;
use http::{HeaderMap, Method};
use url::Url;
use webc::compat::Container;

use crate::{
    bin_factory::BinaryPackage,
    http::{HttpClient, HttpRequest, USER_AGENT},
    runtime::{
        package_loader::PackageLoader,
        resolver::{DistributionInfo, PackageSummary, Resolution, WebcHash},
    },
};

/// The builtin [`PackageLoader`] that is used by the `wasmer` CLI and
/// respects `$WASMER_DIR`.
#[derive(Debug)]
pub struct BuiltinPackageLoader {
    client: Arc<dyn HttpClient + Send + Sync>,
    in_memory: InMemoryCache,

    /// A mapping from hostnames to tokens
    tokens: HashMap<String, String>,

    hash_validation: HashIntegrityValidationMode,
}

/// Defines how to validate package hash integrity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashIntegrityValidationMode {
    /// Do not validate anything.
    /// Best for performance.
    NoValidate,
    /// Compute the image hash and produce a trace warning on hash mismatches.
    WarnOnHashMismatch,
    /// Compute the image hash and fail on a mismatch.
    FailOnHashMismatch,
}

impl BuiltinPackageLoader {
    pub fn new() -> Self {
        BuiltinPackageLoader {
            in_memory: InMemoryCache::default(),
            client: Arc::new(crate::http::default_http_client().unwrap()),
            hash_validation: HashIntegrityValidationMode::NoValidate,
            tokens: HashMap::new(),
        }
    }

    /// Set the validation mode to apply after downloading an image.
    ///
    /// See [`HashIntegrityValidationMode`] for details.
    pub fn with_hash_validation_mode(mut self, mode: HashIntegrityValidationMode) -> Self {
        self.hash_validation = mode;
        self
    }

    pub fn with_http_client(self, client: impl HttpClient + Send + Sync + 'static) -> Self {
        self.with_shared_http_client(Arc::new(client))
    }

    pub fn with_shared_http_client(self, client: Arc<dyn HttpClient + Send + Sync>) -> Self {
        BuiltinPackageLoader { client, ..self }
    }

    pub fn with_tokens<I, K, V>(mut self, tokens: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (hostname, token) in tokens {
            self = self.with_token(hostname, token);
        }

        self
    }

    /// Add an API token that will be used whenever sending requests to a
    /// particular hostname.
    ///
    /// Note that this uses [`Url::authority()`] when looking up tokens, so it
    /// will match both plain hostnames (e.g. `registry.wasmer.io`) and hosts
    /// with a port number (e.g. `localhost:8000`).
    pub fn with_token(mut self, hostname: impl Into<String>, token: impl Into<String>) -> Self {
        self.tokens.insert(hostname.into(), token.into());
        self
    }

    /// Insert a container into the in-memory hash.
    pub fn insert_cached(&self, hash: WebcHash, container: &Container) {
        self.in_memory.save(container, hash);
    }

    #[tracing::instrument(level = "debug", skip_all, fields(pkg.hash=%hash))]
    async fn get_cached(&self, hash: &WebcHash) -> Result<Option<Container>, Error> {
        if let Some(cached) = self.in_memory.lookup(hash) {
            return Ok(Some(cached));
        }

        Ok(None)
    }

    /// Validate image contents with the specified validation mode.
    async fn validate_hash(
        image: &bytes::Bytes,
        mode: HashIntegrityValidationMode,
        info: &DistributionInfo,
    ) -> Result<(), anyhow::Error> {
        let info = info.clone();
        let image = image.clone();
        Self::validate_hash_sync(&image, mode, &info)
    }

    /// Validate image contents with the specified validation mode.
    fn validate_hash_sync(
        image: &[u8],
        mode: HashIntegrityValidationMode,
        info: &DistributionInfo,
    ) -> Result<(), anyhow::Error> {
        match mode {
            HashIntegrityValidationMode::NoValidate => {
                // Nothing to do.
                Ok(())
            }
            HashIntegrityValidationMode::WarnOnHashMismatch => {
                let actual_hash = WebcHash::sha256(image);
                if actual_hash != info.webc_sha256 {
                    tracing::warn!(%info.webc_sha256, %actual_hash, "image hash mismatch - actual image hash does not match the expected hash!");
                }
                Ok(())
            }
            HashIntegrityValidationMode::FailOnHashMismatch => {
                let actual_hash = WebcHash::sha256(image);
                if actual_hash != info.webc_sha256 {
                    Err(ImageHashMismatchError {
                        source: info.webc.to_string(),
                        actual_hash,
                        expected_hash: info.webc_sha256,
                    }
                    .into())
                } else {
                    Ok(())
                }
            }
        }
    }

    #[tracing::instrument(level = "debug", skip_all, fields(%dist.webc, %dist.webc_sha256))]
    async fn download(&self, dist: &DistributionInfo) -> Result<Bytes, Error> {
        let request = HttpRequest {
            headers: self.headers(&dist.webc),
            url: dist.webc.clone(),
            method: Method::GET,
            body: None,
            options: Default::default(),
        };

        tracing::debug!(%request.url, %request.method, "webc_package_download_start");
        tracing::trace!(?request.headers);

        let response = self.client.request(request).await?;

        tracing::trace!(
            %response.status,
            %response.redirected,
            ?response.headers,
            response.len=response.body.as_ref().map(|body| body.len()),
            "Received a response",
        );

        let url = &dist.webc;
        if !response.is_ok() {
            return Err(
                crate::runtime::resolver::utils::http_error(&response).context(format!(
                    "package download failed: GET request to \"{}\" failed with status {}",
                    url, response.status
                )),
            );
        }

        let body = response.body.context("package download failed")?;
        tracing::debug!(%url, "package_download_succeeded");

        let body = bytes::Bytes::from(body);

        Self::validate_hash(&body, self.hash_validation, dist).await?;

        Ok(body)
    }

    fn headers(&self, url: &Url) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("Accept", "application/webc".parse().unwrap());
        headers.insert("User-Agent", USER_AGENT.parse().unwrap());

        if url.has_authority() {
            if let Some(token) = self.tokens.get(url.authority()) {
                let header = format!("Bearer {token}");
                match header.parse() {
                    Ok(header) => {
                        headers.insert(http::header::AUTHORIZATION, header);
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = &e as &dyn std::error::Error,
                            "An error occurred while parsing the authorization header",
                        );
                    }
                }
            }
        }

        headers
    }
}

impl Default for BuiltinPackageLoader {
    fn default() -> Self {
        BuiltinPackageLoader::new()
    }
}

#[async_trait::async_trait]
impl PackageLoader for BuiltinPackageLoader {
    #[tracing::instrument(
        level="debug",
        skip_all,
        fields(
            pkg=%summary.pkg.id,
        ),
    )]
    async fn load(&self, summary: &PackageSummary) -> Result<Container, Error> {
        if let Some(container) = self.get_cached(&summary.dist.webc_sha256).await? {
            tracing::debug!("Cache hit!");
            return Ok(container);
        }

        // looks like we had a cache miss and need to download it manually
        let bytes = self
            .download(&summary.dist)
            .await
            .with_context(|| format!("Unable to download \"{}\"", summary.dist.webc))?;

        let container = Container::from_bytes(bytes)?;
        self.in_memory.save(&container, summary.dist.webc_sha256);

        Ok(container)
    }

    async fn load_package_tree(
        &self,
        root: &Container,
        resolution: &Resolution,
        root_is_local_dir: bool,
    ) -> Result<BinaryPackage, Error> {
        super::load_package_tree(root, self, resolution, root_is_local_dir).await
    }
}

#[derive(Clone, Debug)]
pub struct ImageHashMismatchError {
    source: String,
    expected_hash: WebcHash,
    actual_hash: WebcHash,
}

impl std::fmt::Display for ImageHashMismatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "image hash mismatch! expected hash '{}', but the computed hash is '{}' (source '{}')",
            self.expected_hash, self.actual_hash, self.source,
        )
    }
}

impl std::error::Error for ImageHashMismatchError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheValidationMode {
    /// Just emit a warning for all images where the filename doesn't match
    /// the expected hash.
    WarnOnMismatch,
    /// Remove images from the cache if the filename doesn't match the actual
    /// hash.
    PruneOnMismatch,
}

#[derive(Debug, Default)]
struct InMemoryCache(RwLock<HashMap<WebcHash, Container>>);

impl InMemoryCache {
    fn lookup(&self, hash: &WebcHash) -> Option<Container> {
        self.0.read().unwrap().get(hash).cloned()
    }

    fn save(&self, container: &Container, hash: WebcHash) {
        let mut cache = self.0.write().unwrap();
        cache.entry(hash).or_insert_with(|| container.clone());
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use futures::future::BoxFuture;
    use http::{HeaderMap, StatusCode};
    use wasmer_config::package::PackageId;

    use crate::{
        http::{HttpRequest, HttpResponse},
        runtime::resolver::PackageInfo,
    };

    use super::*;

    const PYTHON: &[u8] = include_bytes!("../../../../../test-assets/python-0.1.0.wasmer");

    #[derive(Debug)]
    pub(crate) struct DummyClient {
        requests: Mutex<Vec<HttpRequest>>,
        responses: Mutex<VecDeque<HttpResponse>>,
    }

    impl DummyClient {
        pub fn with_responses(responses: impl IntoIterator<Item = HttpResponse>) -> Self {
            DummyClient {
                requests: Mutex::new(Vec::new()),
                responses: Mutex::new(responses.into_iter().collect()),
            }
        }
    }

    impl HttpClient for DummyClient {
        fn request(
            &self,
            request: HttpRequest,
        ) -> BoxFuture<'_, Result<HttpResponse, anyhow::Error>> {
            let response = self.responses.lock().unwrap().pop_front().unwrap();
            self.requests.lock().unwrap().push(request);
            Box::pin(async { Ok(response) })
        }
    }

    async fn cache_misses_will_trigger_a_download_internal() {
        let client = Arc::new(DummyClient::with_responses([HttpResponse {
            body: Some(PYTHON.to_vec()),
            redirected: false,
            status: StatusCode::OK,
            headers: HeaderMap::new(),
        }]));
        let loader = BuiltinPackageLoader::new().with_shared_http_client(client.clone());
        let summary = PackageSummary {
            pkg: PackageInfo {
                id: PackageId::new_named("python/python", "0.1.0".parse().unwrap()),
                dependencies: Vec::new(),
                commands: Vec::new(),
                entrypoint: Some("asdf".to_string()),
                filesystem: Vec::new(),
            },
            dist: DistributionInfo {
                webc: "https://wasmer.io/python/python".parse().unwrap(),
                webc_sha256: [0xaa; 32].into(),
            },
        };

        let container = loader.load(&summary).await.unwrap();

        // A HTTP request was sent
        let requests = client.requests.lock().unwrap();
        let request = &requests[0];
        assert_eq!(request.url, summary.dist.webc);
        assert_eq!(request.method, "GET");
        assert_eq!(request.headers.len(), 2);
        assert_eq!(request.headers["Accept"], "application/webc");
        assert_eq!(request.headers["User-Agent"], USER_AGENT);
        // Make sure we got the right package
        let manifest = container.manifest();
        assert_eq!(manifest.entrypoint.as_deref(), Some("python"));
        // it should be cached in memory for next time
        let in_memory = loader.in_memory.0.read().unwrap();
        assert!(in_memory.contains_key(&summary.dist.webc_sha256));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test(flavor = "multi_thread")]
    async fn cache_misses_will_trigger_a_download() {
        cache_misses_will_trigger_a_download_internal().await
    }

    #[cfg(target_arch = "wasm32")]
    #[tokio::test()]
    async fn cache_misses_will_trigger_a_download() {
        cache_misses_will_trigger_a_download_internal().await
    }
}

