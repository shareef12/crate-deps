#![doc = include_str!("../README.md")]

// TODO
//  * Use a local registry index
//  * Should we be specifying a config (it determines where warnings are printed)?

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::mem::ManuallyDrop;

use anyhow::Error as CargoError;
use cargo::core::package_id::PackageId;
use cargo::core::registry::{PackageRegistry, Registry};
use cargo::core::resolver::features::RequestedFeatures;
use cargo::core::resolver::{self, CliFeatures, ResolveOpts, VersionOrdering, VersionPreferences};
use cargo::core::summary::Summary;
use cargo::core::SourceId;
use cargo::core::{Dependency, FeatureValue};
use cargo::sources::source::QueryKind;
use cargo::sources::IndexSummary;
use cargo::util::cache_lock::CacheLockMode;
use cargo::util::config::Config;
use cargo::util::interning::InternedString;
use cargo::util::OptVersionReq;
use thiserror::Error;

const DUMMY_PACKAGE_NAME: &str = "dummy-pkg";
const DUMMY_PACKAGE_VERSION: semver::Version = semver::Version {
    major: 0,
    minor: 1,
    patch: 0,
    pre: semver::Prerelease::EMPTY,
    build: semver::BuildMetadata::EMPTY,
};

#[derive(Debug, Error)]
pub enum Error {
    #[error("cargo error {0:?}")]
    CargoError(#[from] CargoError),
    #[error("couldn't find package: {name} ({version:?})")]
    PackageNotFound {
        name: String,
        version: Option<String>,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

/// A package dependency.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Package {
    pub name: String,
    pub version: String,
}

/// A package dependency resolver.
pub struct Resolver {
    config: ManuallyDrop<Box<Config>>,
    registry: ManuallyDrop<PackageRegistry<'static>>,
    source: SourceId,
}

impl Resolver {
    /// Create a new package dependency resolver using the current Cargo config
    /// and the crates.io index.
    pub fn new() -> Result<Self> {
        let config = Box::new(Config::default()?);
        let source = SourceId::crates_io(&config)?;
        let mut registry = PackageRegistry::new(unsafe { std::mem::transmute(&*config) })?;
        registry.lock_patches();
        Ok(Self {
            config: ManuallyDrop::new(config),
            registry: ManuallyDrop::new(registry),
            source,
        })
    }

    /// Get the dependencies for a single package, merging them into the
    /// specified `dependencies` set.
    pub fn merge_dependencies(
        &mut self,
        package: &str,
        version: Option<&str>,
        dependencies: &mut HashSet<Package>,
    ) -> Result<()> {
        let dep = Dependency::parse(package, version, self.source)?;

        // Get a full summary for the package in question so we can enumerate its
        // features.
        let _lock = self
            .config
            .acquire_package_cache_lock(CacheLockMode::DownloadExclusive)?;
        let summary = get_package_summary(&mut *self.registry, &dep)?;

        // First get a list of all dependencies required if no features are enabled.
        query_dependencies(
            &self.config,
            self.source,
            &mut *self.registry,
            &dep,
            dependencies,
        )?;

        // Try to incrementally enable every feature that may activate an optional
        // dependency, and merge the dependency requirements with our original
        // list. We don't toggle all features at once in case a package declares
        // conflicting features.
        for (feature, fv) in summary.features() {
            if fv.iter().any(|fv| {
                matches!(
                    fv,
                    FeatureValue::Dep { .. } | FeatureValue::DepFeature { .. }
                )
            }) {
                let mut dep = dep.clone();
                dep.set_features([*feature]);
                query_dependencies(
                    &self.config,
                    self.source,
                    &mut *self.registry,
                    &dep,
                    dependencies,
                )?;
            }
        }

        dependencies.remove(&Package {
            name: DUMMY_PACKAGE_NAME.to_string(),
            version: DUMMY_PACKAGE_VERSION.to_string(),
        });

        Ok(())
    }

    /// Get the dependencies for a single package.
    pub fn dependencies(
        &mut self,
        package: &str,
        version: Option<&str>,
    ) -> Result<HashSet<Package>> {
        let mut dependencies = HashSet::new();
        self.merge_dependencies(package, version, &mut dependencies)?;
        Ok(dependencies)
    }
}

impl Drop for Resolver {
    fn drop(&mut self) {
        // The `registry` field holds a reference to `config`. Ensure
        // `registry` is dropped first.
        unsafe {
            ManuallyDrop::drop(&mut self.registry);
            ManuallyDrop::drop(&mut self.config);
        }
    }
}

fn get_package_summary<R: Registry>(registry: &mut R, dep: &Dependency) -> Result<Summary> {
    let mut summaries = Vec::new();
    loop {
        if registry
            .query(dep, QueryKind::Exact, &mut |s: IndexSummary| {
                summaries.push(s.into_summary())
            })
            .is_ready()
        {
            break;
        }
        registry.block_until_ready()?;
    }

    if summaries.is_empty() {
        return Err(Error::PackageNotFound {
            name: dep.package_name().to_string(),
            version: match dep.version_req() {
                OptVersionReq::Any => None,
                OptVersionReq::Req(vr) => Some(vr.to_string()),
                OptVersionReq::Locked(v, _) => Some(v.to_string()),
                OptVersionReq::UpdatePrecise(v, _) => Some(v.to_string()),
            },
        });
    }

    VersionPreferences::default()
        .sort_summaries(&mut summaries, Some(VersionOrdering::MaximumVersionsFirst));

    Ok(summaries.into_iter().next().unwrap())
}

fn query_dependencies<R: Registry>(
    config: &Config,
    source: SourceId,
    registry: &mut R,
    dep: &Dependency,
    all_deps: &mut HashSet<Package>,
) -> Result<()> {
    let pkg_id = PackageId::new(
        InternedString::new(DUMMY_PACKAGE_NAME),
        DUMMY_PACKAGE_VERSION,
        source,
    );
    let summary = Summary::new(
        pkg_id,
        vec![dep.clone()],
        &BTreeMap::new(),
        None::<InternedString>,
        None,
    )?;
    let resolve_opts: ResolveOpts = ResolveOpts::new(
        true,
        RequestedFeatures::CliFeatures(CliFeatures::new_all(true)),
    );
    let version_prefs = VersionPreferences::default();

    let result = resolver::resolve(
        &[(summary, resolve_opts)],
        &[],
        registry,
        &version_prefs,
        Some(config),
    )?;

    all_deps.extend(result.iter().map(|p| Package {
        name: p.name().to_string(),
        version: p.version().to_string(),
    }));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_with_version() {
        let mut resolver = Resolver::new().unwrap();
        let deps = resolver.dependencies("serde", Some("1.0.164")).unwrap();
        eprintln!("{deps:#?}");
    }

    #[test]
    fn serde_without_version() {
        let mut resolver = Resolver::new().unwrap();
        let deps = resolver.dependencies("serde", None).unwrap();
        eprintln!("{deps:#?}");
    }

    #[test]
    fn cargo_without_version() {
        let mut resolver = Resolver::new().unwrap();
        let deps = resolver.dependencies("cargo", None).unwrap();
        eprintln!("{deps:#?}");
    }
}
