use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use walkdir::{DirEntry, WalkDir};

use super::{
    canonicalize_path,
    deps::{normalize_lexical, resolve_manifest_path},
    path_literals::{forbidden_repo_roots, is_forbidden_path_literal, should_scan_path_literals},
    scanner::{scan_source_items, DiscoveredInclusion, DiscoveredStringLiteral, TargetBase},
    InclusionKind, LayerBoundaryError, LayerViolation, READ_ONLY_RELEASE_ALLOWLIST,
};

struct SourceScanContext {
    blockchain_root: PathBuf,
    allowlisted_paths: BTreeSet<PathBuf>,
    forbidden_roots: Vec<PathBuf>,
}

pub(super) fn scan_blockchain_sources(
    repo_root: &Path,
    blockchain_root: &Path,
) -> Result<(usize, usize, Vec<LayerViolation>), LayerBoundaryError> {
    let ctx = SourceScanContext {
        blockchain_root: blockchain_root.to_path_buf(),
        allowlisted_paths: READ_ONLY_RELEASE_ALLOWLIST
            .iter()
            .map(|rel| resolve_manifest_path(repo_root, Path::new(rel)))
            .collect(),
        forbidden_roots: forbidden_repo_roots(repo_root),
    };

    let mut scanned_files = 0usize;
    let mut verified_inclusions = 0usize;
    let mut violations = Vec::new();

    let walker = WalkDir::new(blockchain_root)
        .sort_by_file_name()
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
        let (count, file_violations) = scan_single_file(entry.path(), &ctx)?;
        verified_inclusions += count;
        violations.extend(file_violations);
    }

    Ok((scanned_files, verified_inclusions, violations))
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

fn scan_single_file(
    path: &Path,
    ctx: &SourceScanContext,
) -> Result<(usize, Vec<LayerViolation>), LayerBoundaryError> {
    let source_path = canonicalize_path(path)?;
    let content =
        fs::read_to_string(&source_path).map_err(|source| LayerBoundaryError::ReadSource {
            path: source_path.clone(),
            source,
        })?;
    let scanned = scan_source_items(&source_path, &content)?;
    let manifest_dir = find_manifest_dir(&source_path, &ctx.blockchain_root);
    let boundaries = (ctx.blockchain_root.as_path(), &ctx.allowlisted_paths);
    let mut violations = Vec::new();
    for item in &scanned.inclusions {
        if let Some(violation) =
            check_single_inclusion(&source_path, &manifest_dir, boundaries, item)?
        {
            violations.push(violation);
        }
    }
    if should_scan_path_literals(&source_path, &ctx.blockchain_root) {
        violations.extend(find_path_literal_violations(
            &source_path,
            &manifest_dir,
            &ctx.forbidden_roots,
            &scanned.string_literals,
        ));
    }
    Ok((scanned.inclusions.len(), violations))
}

fn find_path_literal_violations(
    source_file: &Path,
    manifest_dir: &Path,
    forbidden_roots: &[PathBuf],
    literals: &[DiscoveredStringLiteral],
) -> Vec<LayerViolation> {
    literals
        .iter()
        .filter(|item| {
            is_forbidden_path_literal(&item.value, source_file, manifest_dir, forbidden_roots)
        })
        .map(|item| LayerViolation::ForbiddenCrossLayerPathLiteral {
            source_file: source_file.to_path_buf(),
            line: item.line,
            literal: item.value.clone(),
        })
        .collect()
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
