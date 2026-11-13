use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;
use walkdir::{DirEntry, WalkDir};

use super::{
    canonicalize_path,
    scanner::{extract_inclusions, DiscoveredInclusion, TargetBase},
    DependencyEdgeKind, InclusionKind, LayerBoundaryError, LayerViolation, BLOCKCHAIN_REL_DIR,
    READ_ONLY_RELEASE_ALLOWLIST,
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
}

#[derive(Debug)]
pub(super) struct WorkspaceLayout {
    pub(super) repo_root: PathBuf,
    pub(super) blockchain_root: PathBuf,
    pub(super) blockchain_packages: BTreeSet<String>,
    pub(super) outside_packages: BTreeMap<String, PathBuf>,
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

fn resolve_manifest_path(repo_root: &Path, manifest_path: &Path) -> PathBuf {
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
    let violations =
        collect_manifest_violations(&metadata.packages, &blockchain_packages, &outside_packages);
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
    packages: &[MetadataPackage],
    blockchain_packages: &BTreeSet<String>,
    outside_packages: &BTreeMap<String, PathBuf>,
) -> Vec<LayerViolation> {
    let mut violations = Vec::new();
    for pkg in packages {
        if !blockchain_packages.contains(&pkg.name) {
            continue;
        }
        for dep in &pkg.dependencies {
            if let Some(target_manifest) = outside_packages.get(&dep.name) {
                violations.push(LayerViolation::ForbiddenManifestDependency {
                    source_package: pkg.name.clone(),
                    kind: DependencyEdgeKind::from_cargo(dep.kind.as_deref()),
                    target_package: dep.name.clone(),
                    target_manifest: target_manifest.clone(),
                });
            }
        }
    }
    violations
}

pub(super) fn find_resolved_tree_violations(
    tree_output: &str,
    outside_packages: &BTreeMap<String, PathBuf>,
) -> Vec<LayerViolation> {
    let mut seen = BTreeSet::new();
    for line in tree_output.lines() {
        if let Some(first_token) = line.split_whitespace().next() {
            if outside_packages.contains_key(first_token) {
                seen.insert(first_token.to_owned());
            }
        }
    }
    seen.into_iter()
        .map(|target_package| LayerViolation::ForbiddenResolvedTreeEdge { target_package })
        .collect()
}

fn is_skipped_dir(entry: &DirEntry) -> bool {
    entry.file_type().is_dir() && entry.file_name() == "target"
}

fn is_rust_file(entry: &DirEntry) -> bool {
    entry.file_type().is_file()
        && entry.path().extension().and_then(|ext| ext.to_str()) == Some("rs")
}

fn find_manifest_dir(source_file: &Path, blockchain_root: &Path) -> PathBuf {
    let mut current = source_file.parent();
    while let Some(dir) = current {
        if dir.join("Cargo.toml").is_file() {
            return dir.to_path_buf();
        }
        if dir == blockchain_root {
            break;
        }
        current = dir.parent();
    }
    source_file
        .parent()
        .unwrap_or(blockchain_root)
        .to_path_buf()
}

pub(super) fn scan_blockchain_sources(
    repo_root: &Path,
    blockchain_root: &Path,
) -> Result<(usize, usize, Vec<LayerViolation>), LayerBoundaryError> {
    let allowlisted_paths: BTreeSet<PathBuf> = READ_ONLY_RELEASE_ALLOWLIST
        .iter()
        .map(|rel| resolve_manifest_path(repo_root, Path::new(rel)))
        .collect();

    let mut scanned_files = 0usize;
    let mut verified_inclusions = 0usize;
    let mut violations = Vec::new();

    let walker = WalkDir::new(blockchain_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !is_skipped_dir(entry));

    for entry in walker {
        let entry = entry.map_err(|source| LayerBoundaryError::Walk {
            path: blockchain_root.to_path_buf(),
            source,
        })?;
        if !is_rust_file(&entry) {
            continue;
        }
        scanned_files += 1;
        let (count, file_violations) =
            scan_single_file(entry.path(), (blockchain_root, &allowlisted_paths))?;
        verified_inclusions += count;
        violations.extend(file_violations);
    }

    Ok((scanned_files, verified_inclusions, violations))
}

fn scan_single_file(
    path: &Path,
    boundaries: (&Path, &BTreeSet<PathBuf>),
) -> Result<(usize, Vec<LayerViolation>), LayerBoundaryError> {
    let source_path = canonicalize_path(path)?;
    let content =
        fs::read_to_string(&source_path).map_err(|source| LayerBoundaryError::ReadSource {
            path: source_path.clone(),
            source,
        })?;
    let inclusions = extract_inclusions(&source_path, &content)?;
    let manifest_dir = find_manifest_dir(&source_path, boundaries.0);
    let mut violations = Vec::new();
    for item in &inclusions {
        if let Some(violation) =
            check_single_inclusion(&source_path, &manifest_dir, boundaries, item)?
        {
            violations.push(violation);
        }
    }
    Ok((inclusions.len(), violations))
}

fn check_single_inclusion(
    source_file: &Path,
    manifest_dir: &Path,
    boundaries: (&Path, &BTreeSet<PathBuf>),
    item: &DiscoveredInclusion,
) -> Result<Option<LayerViolation>, LayerBoundaryError> {
    let (blockchain_root, allowlisted_paths) = boundaries;
    let base_dir = match item.base {
        TargetBase::SourceParent => source_file.parent().unwrap_or(blockchain_root),
        TargetBase::ManifestDir => manifest_dir,
    };
    let candidate = base_dir.join(&item.raw_target);
    let lexical = normalize_lexical(&candidate);
    if !is_allowed_target(&lexical, item.directive, blockchain_root, allowlisted_paths) {
        return Ok(Some(forbidden_inclusion(source_file, item, lexical)));
    }
    let canonical =
        fs::canonicalize(&candidate).map_err(|source| LayerBoundaryError::UnresolvedTarget {
            source_file: source_file.to_path_buf(),
            line: item.line,
            directive: item.directive,
            target: candidate,
            source,
        })?;
    if !is_allowed_target(
        &canonical,
        item.directive,
        blockchain_root,
        allowlisted_paths,
    ) {
        return Ok(Some(forbidden_inclusion(source_file, item, canonical)));
    }
    Ok(None)
}

fn forbidden_inclusion(
    source_file: &Path,
    item: &DiscoveredInclusion,
    resolved_path: PathBuf,
) -> LayerViolation {
    LayerViolation::ForbiddenFileInclusion {
        source_file: source_file.to_path_buf(),
        line: item.line,
        directive: item.directive,
        raw_target: item.raw_target.clone(),
        resolved_path,
    }
}

fn is_allowed_target(
    resolved: &Path,
    directive: InclusionKind,
    blockchain_root: &Path,
    allowlisted_paths: &BTreeSet<PathBuf>,
) -> bool {
    resolved.starts_with(blockchain_root)
        || (directive.allows_release_pin_allowlist() && allowlisted_paths.contains(resolved))
}
