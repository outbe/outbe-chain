//! Domain-neutral closure checks over strictly parsed verification packs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use eyre::{bail, ensure, Result as EyreResult, WrapErr};
use sha2::{Digest, Sha256};

use super::schema::{
    AssertionStatus, DomainEvidenceV1, DomainPack, MemberDigestV1, SourceIdentityV1,
    VERIFICATION_LEDGER_SCHEMA_VERSION,
};
use super::VerificationLedger;

/// Capture the exact checked-out source and Rust dependency identity.
pub fn capture_source_identity(repo: &Path) -> EyreResult<SourceIdentityV1> {
    let sha = command_stdout(repo, "git", &["rev-parse", "HEAD"])?;
    let tracked = command_output(
        repo,
        "git",
        &["status", "--porcelain", "--untracked-files=no"],
    )?;
    let untracked = command_output(repo, "git", &["ls-files", "--others", "--exclude-standard"])?;
    let metadata = command_output(
        repo,
        "cargo",
        &["metadata", "--locked", "--no-deps", "--format-version", "1"],
    )?;
    Ok(SourceIdentityV1 {
        sha,
        tracked_dirty: !tracked.is_empty(),
        untracked_dirty: !untracked.is_empty(),
        cargo_lock_sha256: sha256_file(&repo.join("Cargo.lock"))?,
        rust_toolchain_sha256: sha256_file(&repo.join("rust-toolchain.toml"))?,
        dependency_metadata_sha256: hex::encode(Sha256::digest(&metadata)),
    })
}

/// Bind an evidence manifest to the exact checkout running the verifier.
pub fn verify_source_checkout(repo: &Path, claimed: &SourceIdentityV1) -> EyreResult<()> {
    ensure!(
        capture_source_identity(repo)? == *claimed,
        "evidence source/toolchain identity differs from the checked-out verifier source"
    );
    Ok(())
}

/// Validate the exact revision/toolchain identity required by a domain pack.
pub fn validate_source_identity(
    source: &SourceIdentityV1,
    exact_revision: bool,
    require_clean_tracked_tree: bool,
    require_clean_untracked_tree: bool,
) -> EyreResult<()> {
    ensure!(
        !require_clean_tracked_tree || !source.tracked_dirty,
        "tracked source tree is dirty"
    );
    ensure!(
        !require_clean_untracked_tree || !source.untracked_dirty,
        "untracked source tree is dirty"
    );
    ensure!(
        !exact_revision || valid_revision(&source.sha),
        "evidence source revision is not exact"
    );
    for (field, digest) in [
        ("Cargo.lock", source.cargo_lock_sha256.as_str()),
        ("Rust toolchain", source.rust_toolchain_sha256.as_str()),
        (
            "dependency metadata",
            source.dependency_metadata_sha256.as_str(),
        ),
    ] {
        ensure!(valid_sha256(digest), "{field} digest is not exact SHA-256");
    }
    Ok(())
}

/// Verify every declared member against the exact bytes under `bundle_root`.
///
/// Domain adapters remain responsible for deciding which members are required
/// and whether unlisted files are allowed. The generic core owns the declared
/// member's content identity.
pub fn verify_member_digests(bundle_root: &Path, members: &[MemberDigestV1]) -> EyreResult<()> {
    for member in members {
        ensure!(
            valid_member_path(&member.path),
            "invalid evidence member path {}",
            member.path
        );
        ensure!(
            valid_sha256(&member.sha256),
            "member {} digest is not exact SHA-256",
            member.path
        );
        let bytes = std::fs::read(bundle_root.join(&member.path))
            .wrap_err_with(|| format!("read evidence member {}", member.path))?;
        let actual_sha256 = hex::encode(Sha256::digest(&bytes));
        ensure!(
            u64::try_from(bytes.len()).ok() == Some(member.length)
                && actual_sha256 == member.sha256,
            "member hash/length mismatch for {}",
            member.path
        );
    }
    Ok(())
}

/// Validate one evidence manifest without claiming that referenced bytes exist.
///
/// Production closure must use [`verify_domain_evidence`] or
/// [`verify_evidence_set`], both of which verify the referenced bundle bytes.
pub fn validate_domain_evidence_manifest(
    pack: &DomainPack,
    evidence: &DomainEvidenceV1,
) -> EyreResult<()> {
    ensure!(
        evidence.schema_version == VERIFICATION_LEDGER_SCHEMA_VERSION,
        "unsupported domain evidence schema"
    );
    ensure!(
        evidence.namespace == pack.verification.namespace,
        "evidence namespace mismatch"
    );
    let identity = &pack.verification.evidence_identity;
    ensure!(
        evidence.artifact_profile == identity.artifact_profile,
        "artifact profile mismatch"
    );
    ensure!(evidence.lane == identity.linux_lane, "Linux lane mismatch");
    ensure!(
        valid_image_digest(&evidence.linux_image_digest),
        "Linux image digest is not an exact sha256 content identity"
    );
    validate_source_identity(
        &evidence.source,
        identity.exact_revision,
        identity.require_clean_tracked_tree,
        identity.require_clean_untracked_tree,
    )?;

    ensure!(
        !evidence.members.is_empty(),
        "domain evidence has no content-addressed members"
    );
    let mut member_paths = BTreeSet::new();
    for member in &evidence.members {
        ensure!(
            valid_member_path(&member.path),
            "invalid evidence member path {}",
            member.path
        );
        ensure!(
            member_paths.insert(member.path.as_str()),
            "duplicate evidence member {}",
            member.path
        );
        ensure!(
            valid_sha256(&member.sha256),
            "member {} digest is not exact SHA-256",
            member.path
        );
    }

    for (label, values) in [
        ("discovered", &evidence.discovery.discovered),
        ("missing", &evidence.discovery.missing),
        ("skipped", &evidence.discovery.skipped),
        ("todo", &evidence.discovery.todo),
        ("quarantined", &evidence.discovery.quarantined),
        ("retried", &evidence.discovery.retried),
    ] {
        ensure!(
            values.iter().collect::<BTreeSet<_>>().len() == values.len(),
            "test discovery repeats {label} IDs"
        );
    }
    let mut discovery = evidence.discovery.clone();
    discovery.normalize();
    ensure!(discovery.missing.is_empty(), "required tests are missing");
    if pack.verification.closure_policy.reject_ignored_or_skipped {
        ensure!(
            discovery.skipped.is_empty(),
            "required tests are ignored or skipped"
        );
    }
    ensure!(
        discovery.todo.is_empty(),
        "required tests contain todo markers"
    );
    ensure!(
        discovery.quarantined.is_empty(),
        "required tests are quarantined"
    );
    ensure!(
        discovery.retried.is_empty(),
        "test evidence used automatic retry"
    );

    let discovered = discovery.discovered.into_iter().collect::<BTreeSet<_>>();
    if pack.verification.closure_policy.reject_unknown_tests {
        for test_id in &discovered {
            ensure!(
                pack.tests.contains_key(test_id),
                "test discovery contains unknown test {test_id}"
            );
        }
    }
    let mut observed = BTreeMap::new();
    let mut referenced_member_paths = BTreeSet::new();
    for test in &evidence.tests {
        ensure!(
            observed.insert(test.test_id.as_str(), test).is_none(),
            "duplicate test evidence {}",
            test.test_id
        );
        ensure!(
            !test.evidence_member_refs.is_empty(),
            "test {} has no evidence member references",
            test.test_id
        );
        let mut test_member_refs = BTreeSet::new();
        for member_ref in &test.evidence_member_refs {
            ensure!(
                test_member_refs.insert(member_ref.as_str()),
                "test {} repeats evidence member {}",
                test.test_id,
                member_ref
            );
            ensure!(
                member_paths.contains(member_ref.as_str()),
                "test {} references unknown evidence member {}",
                test.test_id,
                member_ref
            );
            referenced_member_paths.insert(member_ref.as_str());
        }
        let Some(definition) = pack.tests.get(&test.test_id) else {
            ensure!(
                !pack.verification.closure_policy.reject_unknown_tests,
                "evidence contains unknown test {}",
                test.test_id
            );
            continue;
        };
        let expected_name = definition.name.as_deref().unwrap_or(&test.test_id);
        ensure!(
            test.target == definition.target,
            "test {} used target {}, expected {}",
            test.test_id,
            test.target,
            definition.target
        );
        ensure!(
            test.name == expected_name,
            "test {} used name {}, expected {}",
            test.test_id,
            test.name,
            expected_name
        );
        ensure!(
            test.lane == definition.lane,
            "test {} used lane {}, expected {}",
            test.test_id,
            test.lane,
            definition.lane
        );
        ensure!(
            test.substitutions == definition.substitutions,
            "test {} used substitutions {:?}, expected {:?}",
            test.test_id,
            test.substitutions,
            definition.substitutions
        );
        ensure!(
            discovered.contains(&test.test_id),
            "test {} has evidence but was not discovered",
            test.test_id
        );
        ensure!(
            test.production_interface == definition.production_interface,
            "test {} used evidence interface {}, expected {}",
            test.test_id,
            test.production_interface,
            definition.production_interface
        );
    }

    if pack.verification.closure_policy.require_all_required_tests {
        for (test_id, definition) in &pack.tests {
            if !definition.required {
                continue;
            }
            ensure!(
                discovered.contains(test_id),
                "required test {test_id} was not discovered"
            );
            ensure!(
                !definition.ignored && !definition.skipped,
                "required test {test_id} is ignored or skipped"
            );
            let test = observed
                .get(test_id.as_str())
                .ok_or_else(|| eyre::eyre!("required test {test_id} has no evidence"))?;
            ensure!(
                test.status == AssertionStatus::Pass,
                "required test {test_id} did not pass"
            );
        }
    }
    ensure!(
        referenced_member_paths == member_paths,
        "domain evidence contains unreferenced members"
    );
    Ok(())
}

/// Verify a domain manifest and the exact bytes of all referenced bundle members.
pub fn verify_domain_evidence(
    pack: &DomainPack,
    evidence: &DomainEvidenceV1,
    bundle_root: &Path,
) -> EyreResult<()> {
    validate_domain_evidence_manifest(pack, evidence)?;
    verify_member_digests(bundle_root, &evidence.members)
}

/// One domain evidence manifest paired with the directory containing its bytes.
#[derive(Clone, Copy, Debug)]
pub struct DomainEvidenceBundle<'a> {
    pub evidence: &'a DomainEvidenceV1,
    pub bundle_root: &'a Path,
}

impl<'a> DomainEvidenceBundle<'a> {
    #[must_use]
    pub const fn new(evidence: &'a DomainEvidenceV1, bundle_root: &'a Path) -> Self {
        Self {
            evidence,
            bundle_root,
        }
    }
}

/// Verify a closure set and reject unknown namespaces or mixed identity.
pub fn verify_evidence_set(
    ledger: &VerificationLedger,
    bundles: &[DomainEvidenceBundle<'_>],
) -> EyreResult<()> {
    ensure!(!bundles.is_empty(), "evidence set is empty");
    let first = bundles[0].evidence;
    let mut namespaces = BTreeSet::new();
    for bundle in bundles {
        let domain = bundle.evidence;
        ensure!(
            namespaces.insert(domain.namespace.as_str()),
            "duplicate evidence namespace {}",
            domain.namespace
        );
        ensure!(
            domain.source == first.source,
            "mixed source revision or toolchain identity"
        );
        ensure!(
            domain.artifact_profile == first.artifact_profile,
            "mixed artifact profile evidence"
        );
        ensure!(
            domain.linux_image_digest == first.linux_image_digest,
            "mixed Linux image digest evidence"
        );
        let pack = ledger.domain_pack(&domain.namespace)?;
        verify_domain_evidence(pack, domain, bundle.bundle_root)?;
    }
    Ok(())
}

fn valid_revision(revision: &str) -> bool {
    revision.len() == 40
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_sha256(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_image_digest(digest: &str) -> bool {
    digest.strip_prefix("sha256:").is_some_and(valid_sha256)
}

fn valid_member_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn sha256_file(path: &Path) -> EyreResult<String> {
    let bytes = std::fs::read(path).wrap_err_with(|| format!("read {}", path.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn command_stdout(repo: &Path, program: &str, args: &[&str]) -> EyreResult<String> {
    let bytes = command_output(repo, program, args)?;
    let output =
        String::from_utf8(bytes).wrap_err_with(|| format!("{program} output is not UTF-8"))?;
    let output = output.trim();
    ensure!(!output.is_empty(), "{program} produced empty output");
    Ok(output.to_owned())
}

fn command_output(repo: &Path, program: &str, args: &[&str]) -> EyreResult<Vec<u8>> {
    let output = Command::new(program)
        .args(args)
        .current_dir(repo)
        .output()
        .wrap_err_with(|| format!("run {program} {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}
