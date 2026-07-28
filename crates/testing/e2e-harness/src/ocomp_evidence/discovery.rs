//! Ledger-owned source discovery for stable OCOMP test IDs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use eyre::{bail, ensure, Result, WrapErr};
use walkdir::WalkDir;

use super::ledger::PlanningLedger;
use super::schema::TestDiscoveryV1;

/// Explicit source marker understood by the independent discovery pass.
pub const TEST_ID_MARKER: &str = concat!("OCOMP-TEST-", "ID:");
const TEST_SKIP_MARKER: &str = concat!("OCOMP-TEST-", "SKIP:");
const TEST_TODO_MARKER: &str = concat!("OCOMP-TEST-", "TODO:");
const TEST_QUARANTINE_MARKER: &str = concat!("OCOMP-TEST-", "QUARANTINE:");
const TEST_RETRY_MARKER: &str = concat!("OCOMP-TEST-", "RETRY:");

/// Discover ledger tests and reject duplicate or unknown explicit markers.
pub fn discover(repo: &Path, ledger: &PlanningLedger) -> Result<TestDiscoveryV1> {
    let explicit = scan_explicit_markers(repo)?;
    let known = ledger.test_ids();
    let unknown = explicit
        .keys()
        .filter(|id| !known.contains(*id))
        .cloned()
        .collect::<Vec<_>>();
    ensure!(
        unknown.is_empty(),
        "source contains unknown OCOMP test IDs: {unknown:?}"
    );

    let duplicated = explicit
        .iter()
        .filter(|(_, paths)| paths.len() != 1)
        .map(|(id, paths)| format!("{id}={paths:?}"))
        .collect::<Vec<_>>();
    ensure!(
        duplicated.is_empty(),
        "OCOMP test IDs must have exactly one source marker: {duplicated:?}"
    );

    let mut result = TestDiscoveryV1::default();
    for (test_id, test) in &ledger.tests {
        let planned_path = normalize_relative(&test.planned_path)?;
        let marker_path = explicit
            .get(test_id)
            .and_then(|paths| paths.first())
            .map(|path| path.as_path());
        if marker_path != Some(planned_path.as_path()) {
            result.missing.push(test_id.clone());
            continue;
        }

        result.discovered.push(test_id.clone());
        let contents = std::fs::read_to_string(repo.join(&planned_path))
            .wrap_err_with(|| format!("read discovered test {}", planned_path.display()))?;
        let lowercase = contents.to_ascii_lowercase();
        if has_status_marker(&contents, TEST_SKIP_MARKER, test_id)
            || lowercase.contains("#[ignore]")
            || lowercase.contains("@skip")
            || lowercase.contains("@skipped")
        {
            result.skipped.push(test_id.clone());
        }
        if has_status_marker(&contents, TEST_TODO_MARKER, test_id) || lowercase.contains("@todo") {
            result.todo.push(test_id.clone());
        }
        if has_status_marker(&contents, TEST_QUARANTINE_MARKER, test_id)
            || lowercase.contains("@quarantine")
        {
            result.quarantined.push(test_id.clone());
        }
        if has_status_marker(&contents, TEST_RETRY_MARKER, test_id) || lowercase.contains("@retry")
        {
            result.retried.push(test_id.clone());
        }
    }
    result.normalize();
    Ok(result)
}

fn has_status_marker(contents: &str, marker: &str, test_id: &str) -> bool {
    contents.lines().any(|line| {
        line.split_once(marker)
            .and_then(|(_, suffix)| suffix.split_whitespace().next())
            == Some(test_id)
    })
}

fn scan_explicit_markers(repo: &Path) -> Result<BTreeMap<String, Vec<PathBuf>>> {
    let mut found = BTreeMap::<String, Vec<PathBuf>>::new();
    let roots = [
        "crates/system/ocomp-protocol",
        "crates/core/lysis",
        "crates/core/metadosis",
        "crates/core/tribute",
        "crates/blockchain/node",
        "crates/blockchain/evm",
        "crates/testing/e2e-harness",
        "bin/outbe-ocomp",
    ];
    for root in roots {
        let root = repo.join(root);
        if !root.exists() {
            continue;
        }
        for entry in WalkDir::new(&root).follow_links(false) {
            let entry = entry.wrap_err_with(|| format!("walk {}", root.display()))?;
            if !entry.file_type().is_file() || ignored_path(entry.path()) {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            for line in contents.lines() {
                let Some((_, suffix)) = line.split_once(TEST_ID_MARKER) else {
                    continue;
                };
                let Some(test_id) = suffix.split_whitespace().next() else {
                    bail!(
                        "empty {TEST_ID_MARKER} marker in {}",
                        entry.path().display()
                    );
                };
                let relative = entry
                    .path()
                    .strip_prefix(repo)
                    .wrap_err("discovery path escaped repository")?
                    .to_path_buf();
                found.entry(test_id.to_owned()).or_default().push(relative);
            }
        }
    }
    for paths in found.values_mut() {
        paths.sort();
        paths.dedup();
    }
    Ok(found)
}

fn normalize_relative(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    ensure!(!path.is_absolute(), "planned path is absolute: {value}");
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => normalized.push(part),
            _ => bail!("planned path contains traversal or prefix: {value}"),
        }
    }
    Ok(normalized)
}

fn ignored_path(path: &Path) -> bool {
    const IGNORED: [&str; 5] = [".git", ".codegraph", "target", "node_modules", "vendor"];
    path.components().any(|component| {
        let std::path::Component::Normal(part) = component else {
            return false;
        };
        IGNORED.iter().any(|ignored| part == *ignored)
    })
}

/// Check that discovery is exact and free from non-proving source markers.
pub fn validate_discovery(discovery: &TestDiscoveryV1) -> Result<()> {
    let discovered = discovery.discovered.iter().collect::<BTreeSet<_>>();
    ensure!(
        discovered.len() == discovery.discovered.len(),
        "discovery repeats stable IDs"
    );
    ensure!(
        discovery.skipped.is_empty(),
        "skipped OCOMP tests: {:?}",
        discovery.skipped
    );
    ensure!(
        discovery.todo.is_empty(),
        "todo OCOMP tests: {:?}",
        discovery.todo
    );
    ensure!(
        discovery.quarantined.is_empty(),
        "quarantined OCOMP tests: {:?}",
        discovery.quarantined
    );
    ensure!(
        discovery.retried.is_empty(),
        "automatically retried OCOMP tests: {:?}",
        discovery.retried
    );
    Ok(())
}
