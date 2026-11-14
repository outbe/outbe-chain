use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use super::deps::normalize_lexical;

const FORBIDDEN_REPO_DIRS: [&str; 7] = [
    "crates/core",
    "crates/system",
    "crates/runtime",
    "crates/zk",
    "contracts",
    "testing",
    "bin",
];

const FORBIDDEN_CRATE_LAYERS: [&str; 4] = [
    "crates/core",
    "crates/system",
    "crates/runtime",
    "crates/zk",
];

const FORBIDDEN_SLASH_SEGMENTS: [&str; 3] = ["contracts/", "testing/", "bin/"];

pub(super) fn forbidden_repo_roots(repo_root: &Path) -> Vec<PathBuf> {
    FORBIDDEN_REPO_DIRS
        .iter()
        .map(|rel| {
            let joined = repo_root.join(rel);
            fs::canonicalize(&joined).unwrap_or_else(|_| normalize_lexical(&joined))
        })
        .collect()
}

pub(super) fn should_scan_path_literals(source_file: &Path, blockchain_root: &Path) -> bool {
    if source_file.file_name() == Some(OsStr::new("build.rs")) {
        return true;
    }
    let Ok(rel) = source_file.strip_prefix(blockchain_root) else {
        return false;
    };
    rel.components().any(|comp| comp.as_os_str() == "tests")
}

pub(super) fn is_forbidden_path_literal(
    literal: &str,
    source_file: &Path,
    manifest_dir: &Path,
    forbidden_roots: &[PathBuf],
) -> bool {
    let normalized = literal.replace('\\', "/");
    let candidate = strip_rerun_prefix(&normalized).trim();
    if candidate.is_empty() {
        return false;
    }
    has_forbidden_segment(candidate)
        || resolves_into_forbidden_root(candidate, source_file, manifest_dir, forbidden_roots)
}

fn strip_rerun_prefix(value: &str) -> &str {
    value
        .strip_prefix("cargo::rerun-if-changed=")
        .or_else(|| value.strip_prefix("cargo:rerun-if-changed="))
        .unwrap_or(value)
}

fn has_forbidden_segment(candidate: &str) -> bool {
    FORBIDDEN_CRATE_LAYERS
        .iter()
        .any(|segment| matches_bounded_segment(candidate, segment, true))
        || FORBIDDEN_SLASH_SEGMENTS
            .iter()
            .any(|segment| matches_bounded_segment(candidate, segment, false))
}

fn matches_bounded_segment(haystack: &str, segment: &str, require_right_boundary: bool) -> bool {
    let bytes = haystack.as_bytes();
    for (idx, _) in haystack.match_indices(segment) {
        let left_ok = idx == 0 || matches!(bytes[idx - 1], b'/' | b'=');
        let end = idx + segment.len();
        let right_ok = !require_right_boundary || end == bytes.len() || bytes[end] == b'/';
        if left_ok && right_ok {
            return true;
        }
    }
    false
}

fn resolves_into_forbidden_root(
    candidate: &str,
    source_file: &Path,
    manifest_dir: &Path,
    forbidden_roots: &[PathBuf],
) -> bool {
    let source_dir = source_file.parent().unwrap_or(manifest_dir);
    for base in [source_dir, manifest_dir] {
        let resolved = normalize_lexical(&base.join(candidate));
        if forbidden_roots
            .iter()
            .any(|root| resolved.starts_with(root))
        {
            return true;
        }
    }
    false
}
