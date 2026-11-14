use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;

use super::{
    DependencyEdgeKind, LayerBoundaryError, LayerViolation, BLOCKCHAIN_REL_DIR,
    TRANSITIVE_VENDOR_PATH_ALLOWLIST, VENDOR_PATH_ALLOWLIST,
};

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<MetadataPackage>,
}

#[derive(Debug, Deserialize)]
struct MetadataPackage {
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<MetadataDependency>,
}

#[derive(Debug, Deserialize)]
struct MetadataDependency {
    name: String,
    kind: Option<String>,
    #[serde(default)]
    path: Option<PathBuf>,
}

#[derive(Debug)]
pub(super) struct WorkspaceLayout {
    pub(super) repo_root: PathBuf,
    pub(super) blockchain_root: PathBuf,
    pub(super) blockchain_packages: BTreeSet<String>,
    pub(super) outside_packages: BTreeMap<String, PathBuf>,
}

struct ManifestCheckContext<'a> {
    repo_root: &'a Path,
    blockchain_root: &'a Path,
    outside_packages: &'a BTreeMap<String, PathBuf>,
    vendor_allowlist: BTreeSet<PathBuf>,
}

pub(super) fn normalize_lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

pub(super) fn resolve_manifest_path(repo_root: &Path, manifest_path: &Path) -> PathBuf {
    let joined = if manifest_path.is_absolute() {
        manifest_path.to_path_buf()
    } else {
        repo_root.join(manifest_path)
    };
    fs::canonicalize(&joined).unwrap_or_else(|_| normalize_lexical(&joined))
}

pub(super) fn inspect_metadata_edges(
    repo_root: &Path,
    metadata_json: &str,
) -> Result<(WorkspaceLayout, Vec<LayerViolation>), LayerBoundaryError> {
    let metadata: CargoMetadata = serde_json::from_str(metadata_json)?;
    let blockchain_root = resolve_manifest_path(repo_root, &repo_root.join(BLOCKCHAIN_REL_DIR));
    let (blockchain_packages, outside_packages) =
        partition_packages(repo_root, &blockchain_root, &metadata.packages);
    let violations = collect_manifest_violations(
        (repo_root, &blockchain_root),
        &metadata.packages,
        &blockchain_packages,
        &outside_packages,
    );
    Ok((
        WorkspaceLayout {
            repo_root: repo_root.to_path_buf(),
            blockchain_root,
            blockchain_packages,
            outside_packages,
        },
        violations,
    ))
}

fn partition_packages(
    repo_root: &Path,
    blockchain_root: &Path,
    packages: &[MetadataPackage],
) -> (BTreeSet<String>, BTreeMap<String, PathBuf>) {
    let mut blockchain_packages = BTreeSet::new();
    let mut outside_packages = BTreeMap::new();
    for pkg in packages {
        let resolved = resolve_manifest_path(repo_root, &pkg.manifest_path);
        if resolved.starts_with(blockchain_root) {
            blockchain_packages.insert(pkg.name.clone());
        } else {
            outside_packages.insert(pkg.name.clone(), resolved);
        }
    }
    (blockchain_packages, outside_packages)
}

fn collect_manifest_violations(
    roots: (&Path, &Path),
    packages: &[MetadataPackage],
    blockchain_packages: &BTreeSet<String>,
    outside_packages: &BTreeMap<String, PathBuf>,
) -> Vec<LayerViolation> {
    let (repo_root, blockchain_root) = roots;
    let ctx = ManifestCheckContext {
        repo_root,
        blockchain_root,
        outside_packages,
        vendor_allowlist: VENDOR_PATH_ALLOWLIST
            .iter()
            .map(|rel| resolve_manifest_path(repo_root, Path::new(rel)))
            .collect(),
    };
    let mut violations = Vec::new();
    for pkg in packages {
        if !blockchain_packages.contains(&pkg.name) {
            continue;
        }
        let pkg_manifest = resolve_manifest_path(repo_root, &pkg.manifest_path);
        let pkg_dir = pkg_manifest.parent().unwrap_or(blockchain_root);
        for dep in &pkg.dependencies {
            check_single_dependency(&ctx, (&pkg.name, pkg_dir), dep, &mut violations);
        }
    }
    violations
}

fn check_single_dependency(
    ctx: &ManifestCheckContext<'_>,
    pkg: (&str, &Path),
    dep: &MetadataDependency,
    violations: &mut Vec<LayerViolation>,
) {
    let (pkg_name, pkg_dir) = pkg;
    let kind = DependencyEdgeKind::from_cargo(dep.kind.as_deref());
    if let Some(target_manifest) = ctx.outside_packages.get(&dep.name) {
        violations.push(LayerViolation::ForbiddenManifestDependency {
            source_package: pkg_name.to_owned(),
            kind,
            target_package: dep.name.clone(),
            target_manifest: target_manifest.clone(),
        });
    }
    if let Some(raw_path) = &dep.path {
        let resolved_path = resolve_local_dep_path(ctx.repo_root, pkg_dir, raw_path);
        if !is_allowed_local_dep_path(&resolved_path, ctx.blockchain_root, &ctx.vendor_allowlist) {
            violations.push(LayerViolation::ForbiddenLocalPathDependency {
                source_package: pkg_name.to_owned(),
                kind,
                target_package: dep.name.clone(),
                resolved_path,
            });
        }
    }
}

fn resolve_local_dep_path(repo_root: &Path, pkg_dir: &Path, dep_path: &Path) -> PathBuf {
    let joined = if dep_path.is_absolute() {
        dep_path.to_path_buf()
    } else if dep_path.starts_with("..") {
        pkg_dir.join(dep_path)
    } else {
        repo_root.join(dep_path)
    };
    fs::canonicalize(&joined).unwrap_or_else(|_| normalize_lexical(&joined))
}

fn is_allowed_local_dep_path(
    resolved_path: &Path,
    blockchain_root: &Path,
    vendor_allowlist: &BTreeSet<PathBuf>,
) -> bool {
    (resolved_path != blockchain_root && resolved_path.starts_with(blockchain_root))
        || vendor_allowlist.contains(resolved_path)
}

pub(super) fn find_resolved_tree_violations(
    roots: (&Path, &Path),
    tree_output: &str,
    outside_packages: &BTreeMap<String, PathBuf>,
) -> Vec<LayerViolation> {
    let (repo_root, blockchain_root) = roots;
    let vendor_allowlist: BTreeSet<PathBuf> = TRANSITIVE_VENDOR_PATH_ALLOWLIST
        .iter()
        .map(|rel| resolve_manifest_path(repo_root, Path::new(rel)))
        .collect();
    let mut seen = BTreeSet::new();
    for line in tree_output.lines() {
        let Some(first_token) = line.split_whitespace().next() else {
            continue;
        };
        if outside_packages.contains_key(first_token) {
            seen.insert(first_token.to_owned());
        } else if let Some(local_dir) = extract_local_tree_path(line) {
            let resolved = resolve_manifest_path(repo_root, Path::new(local_dir));
            if !is_allowed_local_dep_path(&resolved, blockchain_root, &vendor_allowlist) {
                seen.insert(first_token.to_owned());
            }
        }
    }
    seen.into_iter()
        .map(|target_package| LayerViolation::ForbiddenResolvedTreeEdge { target_package })
        .collect()
}

fn extract_local_tree_path(line: &str) -> Option<&str> {
    let mut rest = line;
    while let Some(open) = rest.find('(') {
        let after_open = &rest[open + 1..];
        let close = after_open.find(')')?;
        let inner = &after_open[..close];
        if inner.starts_with('/') || inner.as_bytes().get(1) == Some(&b':') {
            return Some(inner);
        }
        rest = &after_open[close + 1..];
    }
    None
}
