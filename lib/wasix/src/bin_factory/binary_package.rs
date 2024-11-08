use std::{path::Path, sync::Arc};

use anyhow::Context;
use derivative::*;
use once_cell::sync::OnceCell;
use sha2::Digest;
use virtual_fs::FileSystem;
use wasmer_config::package::{PackageHash, PackageId, PackageSource};
use webc::{compat::SharedBytes, Container};

use crate::{
    runners::MappedDirectory,
    runtime::resolver::{PackageInfo, ResolveError},
    Runtime,
};
use wasmer_types::ModuleHash;

#[derive(Derivative, Clone)]
#[derivative(Debug)]
pub struct BinaryPackageCommand {
    name: String,
    metadata: webc::metadata::Command,
    #[derivative(Debug = "ignore")]
    pub(crate) atom: SharedBytes,
    hash: ModuleHash,
}

impl BinaryPackageCommand {
    pub fn new(
        name: String,
        metadata: webc::metadata::Command,
        atom: SharedBytes,
        hash: ModuleHash,
    ) -> Self {
        Self {
            name,
            metadata,
            atom,
            hash,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn metadata(&self) -> &webc::metadata::Command {
        &self.metadata
    }

    /// Get a reference to this [`BinaryPackageCommand`]'s atom.
    ///
    /// The address of the returned slice is guaranteed to be stable and live as
    /// long as the [`BinaryPackageCommand`].
    pub fn atom(&self) -> &[u8] {
        &self.atom
    }

    pub fn hash(&self) -> &ModuleHash {
        &self.hash
    }
}

/// A WebAssembly package that has been loaded into memory.
#[derive(Derivative, Clone)]
#[derivative(Debug)]
pub struct BinaryPackage {
    pub id: PackageId,
    /// Includes the ids of all the packages in the tree
    pub package_ids: Vec<PackageId>,

    pub when_cached: Option<u128>,
    /// The name of the [`BinaryPackageCommand`] which is this package's
    /// entrypoint.
    pub entrypoint_cmd: Option<String>,
    pub hash: OnceCell<ModuleHash>,
    pub webc_fs: Arc<dyn FileSystem + Send + Sync>,
    pub commands: Vec<BinaryPackageCommand>,
    pub uses: Vec<String>,
    pub file_system_memory_footprint: u64,

    pub additional_host_mapped_directories: Vec<MappedDirectory>,
}

impl BinaryPackage {
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn from_dir(
        dir: &Path,
        rt: &(dyn Runtime + Send + Sync),
    ) -> Result<Self, anyhow::Error> {
        let source = rt.source();

        // since each package must be in its own directory, hash of the `dir` should provide a good enough
        // unique identifier for the package
        let hash = sha2::Sha256::digest(dir.display().to_string().as_bytes()).into();
        let id = PackageId::Hash(PackageHash::from_sha256_bytes(hash));

        let manifest_path = dir.join("wasmer.toml");
        let webc = webc::wasmer_package::Package::from_manifest(&manifest_path)?;
        let container = Container::from(webc);
        let manifest = container.manifest();

        let root = PackageInfo::from_manifest(id, manifest, container.version())?;
        let root_id = root.id.clone();

        let resolution = crate::runtime::resolver::resolve(&root_id, &root, &*source).await?;
        let mut pkg = rt
            .package_loader()
            .load_package_tree(&container, &resolution, true)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        // HACK: webc has no way to return its deserialized manifest to us, so we need to do it again here
        // We already read and parsed the manifest once, so it'll succeed again. Unwrapping is safe at this point.
        let wasmer_toml = std::fs::read_to_string(&manifest_path).unwrap();
        let wasmer_toml: wasmer_config::package::Manifest = toml::from_str(&wasmer_toml).unwrap();
        pkg.additional_host_mapped_directories.extend(
            wasmer_toml
                .fs
                .into_iter()
                .map(|(guest, host)| {
                    anyhow::Ok(MappedDirectory {
                        host: dir.join(host).canonicalize()?,
                        guest,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter(),
        );

        Ok(pkg)
    }

    /// Load a [`webc::Container`] and all its dependencies into a
    /// [`BinaryPackage`].
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn from_webc(
        container: &Container,
        rt: &(dyn Runtime + Send + Sync),
    ) -> Result<Self, anyhow::Error> {
        let source = rt.source();

        let manifest = container.manifest();
        let id = PackageInfo::package_id_from_manifest(manifest)?
            .or_else(|| {
                container
                    .webc_hash()
                    .map(|hash| PackageId::Hash(PackageHash::from_sha256_bytes(hash)))
            })
            .ok_or_else(|| anyhow::Error::msg("webc file did not provide its hash"))?;

        let root = PackageInfo::from_manifest(id, manifest, container.version())?;
        let root_id = root.id.clone();

        let resolution = crate::runtime::resolver::resolve(&root_id, &root, &*source).await?;
        let pkg = rt
            .package_loader()
            .load_package_tree(container, &resolution, false)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        Ok(pkg)
    }

    /// Load a [`BinaryPackage`] and all its dependencies from a registry.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn from_registry(
        specifier: &PackageSource,
        runtime: &(dyn Runtime + Send + Sync),
    ) -> Result<Self, anyhow::Error> {
        let source = runtime.source();
        let root_summary =
            source
                .latest(specifier)
                .await
                .map_err(|error| ResolveError::Registry {
                    package: specifier.clone(),
                    error,
                })?;
        let root = runtime.package_loader().load(&root_summary).await?;
        let id = root_summary.package_id();

        let resolution = crate::runtime::resolver::resolve(&id, &root_summary.pkg, &source)
            .await
            .context("Dependency resolution failed")?;
        let pkg = runtime
            .package_loader()
            .load_package_tree(&root, &resolution, false)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        Ok(pkg)
    }

    pub fn get_command(&self, name: &str) -> Option<&BinaryPackageCommand> {
        self.commands.iter().find(|cmd| cmd.name() == name)
    }

    /// Resolve the entrypoint command name to a [`BinaryPackageCommand`].
    pub fn get_entrypoint_command(&self) -> Option<&BinaryPackageCommand> {
        self.entrypoint_cmd
            .as_deref()
            .and_then(|name| self.get_command(name))
    }

    /// Get the bytes for the entrypoint command.
    #[deprecated(
        note = "Use BinaryPackage::get_entrypoint_cmd instead",
        since = "0.22.0"
    )]
    pub fn entrypoint_bytes(&self) -> Option<&[u8]> {
        self.get_entrypoint_command().map(|entry| entry.atom())
    }

    /// Get a hash for this binary package.
    ///
    /// Usually the hash of the entrypoint.
    pub fn hash(&self) -> ModuleHash {
        *self.hash.get_or_init(|| {
            if let Some(cmd) = self.get_entrypoint_command() {
                cmd.hash
            } else {
                ModuleHash::xxhash(self.id.to_string())
            }
        })
    }
}
