// TODO
//  * Use a local registry index
//  * Should we be specifying a config (it determines where warnings are printed)?

use std::collections::BTreeMap;
use std::collections::HashSet;

use anyhow::Error as CargoError;
use cargo::core::package_id::PackageId;
use cargo::core::registry::{PackageRegistry, Registry};
use cargo::core::resolver::features::RequestedFeatures;
use cargo::core::resolver::{self, CliFeatures, ResolveOpts, VersionOrdering, VersionPreferences};
use cargo::core::source::SourceId;
use cargo::core::summary::Summary;
use cargo::core::{Dependency, FeatureValue, QueryKind};
use cargo::util::config::Config;
use cargo::util::interning::InternedString;
use cargo::util::OptVersionReq;
use thiserror::Error;

const DUMMY_PACKAGE_NAME: &str = "dummy-pkg";
const DUMMY_PACKAGE_VERSION: &str = "0.1.0";

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

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Package {
    pub name: String,
    pub version: String,
}

pub fn dependencies(package: &str, version: Option<&str>) -> Result<HashSet<Package>> {
    let config = Config::default()?;
    let _lock = config.acquire_package_cache_lock()?;

    let source = SourceId::crates_io(&config)?;
    let mut registry = PackageRegistry::new(&config)?;
    registry.lock_patches();

    // Get a full summary for the package in question so we can enumerate its
    // features.
    let dep = Dependency::parse(package, version, source)?;
    let summary = get_package_summary(&mut registry, &dep)?;

    // First get a list of all dependencies required if no features are enabled.
    let mut all_deps = HashSet::new();
    query_dependencies(&config, source, &mut registry, &dep, &mut all_deps)?;

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
            query_dependencies(&config, source, &mut registry, &dep, &mut all_deps)?;
        }
    }

    all_deps.remove(&Package {
        name: DUMMY_PACKAGE_NAME.to_string(),
        version: DUMMY_PACKAGE_VERSION.to_string(),
    });

    Ok(all_deps)
}

fn get_package_summary<R: Registry>(registry: &mut R, dep: &Dependency) -> Result<Summary> {
    let mut summaries = Vec::new();
    loop {
        if registry
            .query(dep, QueryKind::Exact, &mut |s: Summary| summaries.push(s))
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
            },
        });
    }

    VersionPreferences::default().sort_summaries(
        &mut summaries,
        VersionOrdering::MaximumVersionsFirst,
        false,
    );

    Ok(summaries.into_iter().next().unwrap())
}

fn query_dependencies<R: Registry>(
    config: &Config,
    source: SourceId,
    registry: &mut R,
    dep: &Dependency,
    all_deps: &mut HashSet<Package>,
) -> Result<()> {
    let summary = Summary::new(
        config,
        PackageId::new(DUMMY_PACKAGE_NAME, DUMMY_PACKAGE_VERSION, source)?,
        vec![dep.clone()],
        &BTreeMap::new(),
        None::<InternedString>,
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
        Some(&config),
        false,
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
        let deps = dependencies("serde", Some("1.0.164")).unwrap();
        eprintln!("{:#?}", deps);
    }

    #[test]
    fn serde_without_version() {
        let deps = dependencies("serde", None).unwrap();
        eprintln!("{:#?}", deps);
    }

    #[test]
    fn cargo_without_version() {
        let deps = dependencies("cargo", None).unwrap();
        eprintln!("{:#?}", deps);
    }
}
