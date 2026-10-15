//! Derive and verify Metadosis evidence from runner-owned facts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use eyre::{ensure, Result, WrapErr};
use outbe_chain_constants::GenesisProtocolParametersV1;
#[cfg(feature = "ocomp-integration")]
use outbe_metadosis::config::{OcompForkInstallClassification, OcompForkInstallV1};
#[cfg(all(feature = "ocomp-integration", test))]
use outbe_metadosis::test_support::ForkInstallScenario;
#[cfg(feature = "ocomp-integration")]
use outbe_ocomp_protocol::profile::poc_schema_limits;
#[cfg(feature = "ocomp-integration")]
use outbe_primitives::OutbeHeader;
use walkdir::WalkDir;

use super::container::{retained_provenance_members, PROVENANCE_MEMBER};
use super::runner::{
    command_plan, digest_member, retained_p0_member, write_new_json, METADOSIS_FRESH_DEVNET_NAME,
    METADOSIS_FRESH_DEVNET_TARGET, METADOSIS_FRESH_DEVNET_TEST_ID, METADOSIS_P0_NAME,
    METADOSIS_P0_TARGET, METADOSIS_P0_TEST_ID, RUNNER_RECEIPT_MEMBER,
};
use super::schema::{
    CommandReceiptV1, MetadosisRunReceiptV1, TestRunReceiptV1, METADOSIS_NAMESPACE,
    METADOSIS_RUN_RECEIPT_KIND, METADOSIS_RUN_RECEIPT_SCHEMA_VERSION,
};
use crate::metadosis_p0::{
    capture_outbe_chain_feature_tree, verify_metadosis_p0_parity, MetadosisP0Case,
    MetadosisP0CaseInput, METADOSIS_P0_SCENARIO,
};
use crate::verification_ledger::{
    verify_domain_evidence, verify_member_digests, verify_source_checkout, AssertionStatus,
    DomainEvidenceV1, EvidenceTestV1, MemberDigestV1, TestDefinitionV1, TestDiscoveryV1,
    VerificationLedger, VERIFICATION_LEDGER_SCHEMA_VERSION,
};

mod fresh_devnet;

use fresh_devnet::verify_fresh_devnet_process;
#[cfg(all(feature = "ocomp-integration", test))]
use fresh_devnet::verify_price_oracle_evidence;

pub fn assemble(
    repo: &Path,
    index_path: &Path,
    bundle_root: &Path,
    receipt_path: &Path,
    output: &Path,
) -> Result<DomainEvidenceV1> {
    let receipt = decode_receipt(receipt_path)?;
    let evidence = derive(repo, index_path, bundle_root, receipt_path, &receipt)?;
    write_new_json(output, &evidence)?;
    Ok(evidence)
}

pub fn verify(
    repo: &Path,
    index_path: &Path,
    bundle_root: &Path,
    evidence_path: &Path,
) -> Result<()> {
    let evidence: DomainEvidenceV1 = serde_json::from_slice(
        &std::fs::read(evidence_path)
            .wrap_err_with(|| format!("read {}", evidence_path.display()))?,
    )
    .wrap_err("decode Metadosis DomainEvidenceV1")?;
    require_runner_derivation_member(&evidence)?;
    verify_source_checkout(repo, &evidence.source)?;
    let ledger = VerificationLedger::parse(index_path)?;
    let pack = ledger.domain_pack(METADOSIS_NAMESPACE)?;
    verify_domain_evidence(pack, &evidence, bundle_root)?;
    let receipt_path = bundle_root.join(RUNNER_RECEIPT_MEMBER);
    let receipt = decode_receipt(&receipt_path)?;
    let derived = derive(repo, index_path, bundle_root, &receipt_path, &receipt)?;
    ensure!(
        evidence == derived,
        "Metadosis evidence was not derived exactly from the retained runner receipt"
    );
    Ok(())
}

pub fn require_runner_derivation_member(evidence: &DomainEvidenceV1) -> Result<()> {
    ensure!(
        evidence
            .members
            .iter()
            .any(|member| member.path == RUNNER_RECEIPT_MEMBER),
        "Metadosis evidence has no content-addressed runner-receipt.json"
    );
    ensure!(
        evidence
            .members
            .iter()
            .any(|member| member.path == PROVENANCE_MEMBER),
        "Metadosis evidence has no content-addressed container provenance"
    );
    ensure!(
        evidence.tests.iter().all(|test| {
            test.evidence_member_refs
                .iter()
                .any(|member| member == RUNNER_RECEIPT_MEMBER)
                && test
                    .evidence_member_refs
                    .iter()
                    .any(|member| member == PROVENANCE_MEMBER)
        }),
        "every Metadosis result must bind to runner receipt and container provenance"
    );
    Ok(())
}

fn derive(
    repo: &Path,
    index_path: &Path,
    bundle_root: &Path,
    receipt_path: &Path,
    receipt: &MetadosisRunReceiptV1,
) -> Result<DomainEvidenceV1> {
    validate_receipt_header(receipt)?;
    let ledger = VerificationLedger::parse(index_path)?;
    let pack = ledger.domain_pack(METADOSIS_NAMESPACE)?;
    ensure!(
        receipt.source.sha.len() == 40,
        "runner source revision is not exact"
    );
    ensure!(
        receipt.artifact_profile == pack.verification.evidence_identity.artifact_profile,
        "runner artifact profile mismatch"
    );
    ensure!(
        receipt.lane == pack.verification.evidence_identity.linux_lane,
        "runner lane mismatch"
    );
    let retained_receipt = receipt_path
        .canonicalize()
        .wrap_err("canonicalize runner receipt")?;
    ensure!(
        retained_receipt
            == bundle_root
                .join(RUNNER_RECEIPT_MEMBER)
                .canonicalize()
                .wrap_err("canonicalize bundle runner receipt")?,
        "runner receipt must be the bundle's exact runner-receipt.json"
    );

    let mut receipts = BTreeMap::new();
    for test in &receipt.tests {
        ensure!(
            receipts.insert(test.test_id.as_str(), test).is_none(),
            "duplicate runner receipt for {}",
            test.test_id
        );
    }
    ensure!(
        receipts.keys().copied().collect::<BTreeSet<_>>()
            == pack
                .tests
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
        "runner receipt does not partition the Metadosis test catalog exactly once"
    );

    let runner_member = digest_member(bundle_root, RUNNER_RECEIPT_MEMBER)?;
    let mut all_members = BTreeMap::new();
    insert_member(&mut all_members, runner_member.clone())?;
    let provenance_members = retained_provenance_members(repo, index_path, bundle_root, receipt)?;
    let provenance_refs = provenance_members
        .iter()
        .map(|member| member.path.clone())
        .collect::<Vec<_>>();
    for member in provenance_members {
        insert_member(&mut all_members, member)?;
    }
    let mut discovery = TestDiscoveryV1::default();
    let mut tests = Vec::with_capacity(pack.tests.len());
    for (test_id, definition) in &pack.tests {
        let receipt = receipts
            .get(test_id.as_str())
            .expect("exact catalog equality");
        let plan = command_plan(test_id, definition, bundle_root)?;
        ensure!(
            receipt.execution.argv == plan.execution,
            "{test_id} execution argv differs from the fixed command plan"
        );
        validate_command(bundle_root, &receipt.execution)?;
        let mut refs = vec![
            RUNNER_RECEIPT_MEMBER.to_owned(),
            receipt.execution.stdout.path.clone(),
            receipt.execution.stderr.path.clone(),
        ];
        refs.extend(provenance_refs.iter().cloned());
        insert_member(&mut all_members, receipt.execution.stdout.clone())?;
        insert_member(&mut all_members, receipt.execution.stderr.clone())?;

        let mut collected = CollectedMembers {
            refs,
            all: &mut all_members,
        };
        let (discovered, ignored) = if test_id == METADOSIS_P0_TEST_ID {
            p0_discovery(repo, bundle_root, definition, receipt, &mut collected)?
        } else if test_id == METADOSIS_FRESH_DEVNET_TEST_ID {
            fresh_devnet_discovery(bundle_root, definition, receipt, &mut collected)?
        } else {
            cargo_discovery(
                bundle_root,
                (test_id, definition),
                receipt,
                plan.discovery,
                &mut collected,
            )?
        };
        let mut refs = collected.refs;

        if discovered {
            discovery.discovered.push(test_id.clone());
        } else {
            discovery.missing.push(test_id.clone());
        }
        if ignored {
            discovery.skipped.push(test_id.clone());
        }
        let status = derive_status(
            discovered,
            ignored,
            receipt.execution.succeeded(),
            receipt.execution.exit_code.is_none(),
        );
        refs.sort();
        refs.dedup();
        tests.push(EvidenceTestV1 {
            test_id: test_id.clone(),
            target: definition.target.clone(),
            name: definition.name.clone().unwrap_or_else(|| test_id.clone()),
            lane: definition.lane.clone(),
            production_interface: definition.production_interface.clone(),
            substitutions: definition.substitutions.clone(),
            status,
            evidence_member_refs: refs,
        });
    }
    discovery.normalize();
    let members = all_members.into_values().collect::<Vec<_>>();
    verify_member_digests(bundle_root, &members)?;
    let evidence = DomainEvidenceV1 {
        schema_version: VERIFICATION_LEDGER_SCHEMA_VERSION,
        namespace: METADOSIS_NAMESPACE.to_owned(),
        source: receipt.source.clone(),
        artifact_profile: receipt.artifact_profile.clone(),
        lane: receipt.lane.clone(),
        linux_image_digest: receipt.linux_image_digest.clone(),
        discovery,
        tests,
        members,
    };
    require_runner_derivation_member(&evidence)?;
    verify_domain_evidence(pack, &evidence, bundle_root)?;
    Ok(evidence)
}

struct CollectedMembers<'a> {
    refs: Vec<String>,
    all: &'a mut BTreeMap<String, MemberDigestV1>,
}

impl CollectedMembers<'_> {
    fn add(&mut self, member: MemberDigestV1) -> Result<()> {
        self.refs.push(member.path.clone());
        insert_member(self.all, member)
    }
}

fn bind_process_artifacts(
    receipt: &TestRunReceiptV1,
    required: Vec<MemberDigestV1>,
    label: &str,
    collected: &mut CollectedMembers<'_>,
) -> Result<()> {
    let recorded = receipt
        .process_artifacts
        .iter()
        .map(|member| member.path.as_str())
        .collect::<BTreeSet<_>>();
    let required_paths = required
        .iter()
        .map(|member| member.path.as_str())
        .collect::<BTreeSet<_>>();
    ensure!(
        recorded == required_paths,
        "{label} process artifacts are incomplete or contain unverified members"
    );
    for member in required {
        collected.add(member)?;
    }
    Ok(())
}

fn p0_discovery(
    repo: &Path,
    bundle_root: &Path,
    definition: &TestDefinitionV1,
    receipt: &TestRunReceiptV1,
    collected: &mut CollectedMembers<'_>,
) -> Result<(bool, bool)> {
    ensure!(
        definition.target == METADOSIS_P0_TARGET
            && definition.name.as_deref() == Some(METADOSIS_P0_NAME),
        "P0 catalog binding drift"
    );
    ensure!(
        receipt.discovery.is_none(),
        "P0 process target cannot use Cargo discovery"
    );
    let required = verify_p0_process(repo, bundle_root, receipt)?;
    bind_process_artifacts(receipt, required, "P0", collected)?;
    Ok((true, false))
}

fn fresh_devnet_discovery(
    bundle_root: &Path,
    definition: &TestDefinitionV1,
    receipt: &TestRunReceiptV1,
    collected: &mut CollectedMembers<'_>,
) -> Result<(bool, bool)> {
    ensure!(
        definition.target == METADOSIS_FRESH_DEVNET_TARGET
            && definition.name.as_deref() == Some(METADOSIS_FRESH_DEVNET_NAME),
        "fresh-devnet catalog binding drift"
    );
    ensure!(
        receipt.discovery.is_none(),
        "fresh-devnet process target cannot use Cargo discovery"
    );
    let required = verify_fresh_devnet_process(bundle_root, receipt)?;
    bind_process_artifacts(receipt, required, "fresh-devnet", collected)?;
    Ok((true, false))
}

fn cargo_discovery(
    bundle_root: &Path,
    (test_id, definition): (&str, &TestDefinitionV1),
    receipt: &TestRunReceiptV1,
    plan_discovery: Option<(Vec<String>, Vec<String>)>,
    collected: &mut CollectedMembers<'_>,
) -> Result<(bool, bool)> {
    ensure!(
        receipt.process_artifacts.is_empty(),
        "{test_id} contains unexpected process artifacts"
    );
    let (normal_plan, ignored_plan) = plan_discovery.expect("non-P0 command has Cargo discovery");
    let observed = receipt
        .discovery
        .as_ref()
        .ok_or_else(|| eyre::eyre!("{test_id} has no Cargo discovery receipt"))?;
    ensure!(
        observed.normal.argv == normal_plan && observed.ignored.argv == ignored_plan,
        "{test_id} discovery argv differs from the fixed command plan"
    );
    validate_command(bundle_root, &observed.normal)?;
    validate_command(bundle_root, &observed.ignored)?;
    for member in [
        &observed.normal.stdout,
        &observed.normal.stderr,
        &observed.ignored.stdout,
        &observed.ignored.stderr,
    ] {
        collected.add(member.clone())?;
    }
    let name = definition.name.as_deref().unwrap_or(test_id);
    Ok((
        observed.normal.succeeded()
            && cargo_list_contains(bundle_root, &observed.normal.stdout.path, name)?,
        observed.ignored.succeeded()
            && cargo_list_contains(bundle_root, &observed.ignored.stdout.path, name)?,
    ))
}

fn validate_receipt_header(receipt: &MetadosisRunReceiptV1) -> Result<()> {
    ensure!(
        receipt.schema_version == METADOSIS_RUN_RECEIPT_SCHEMA_VERSION,
        "unsupported Metadosis runner receipt schema"
    );
    ensure!(
        receipt.kind == METADOSIS_RUN_RECEIPT_KIND,
        "unexpected Metadosis runner receipt kind"
    );
    ensure!(
        receipt.namespace == METADOSIS_NAMESPACE,
        "unexpected Metadosis runner namespace"
    );
    Ok(())
}

fn validate_command(bundle_root: &Path, receipt: &CommandReceiptV1) -> Result<()> {
    ensure!(
        receipt.attempt == 1,
        "automatic or manual test retry is forbidden"
    );
    ensure!(
        receipt.started_at_unix_ms <= receipt.finished_at_unix_ms,
        "command timestamps are reversed"
    );
    verify_member_digests(
        bundle_root,
        &[receipt.stdout.clone(), receipt.stderr.clone()],
    )
}

fn derive_status(
    discovered: bool,
    ignored: bool,
    execution_succeeded: bool,
    signaled: bool,
) -> AssertionStatus {
    if !discovered {
        AssertionStatus::Missing
    } else if ignored {
        AssertionStatus::Skipped
    } else if execution_succeeded {
        AssertionStatus::Pass
    } else if signaled {
        AssertionStatus::InfraError
    } else {
        AssertionStatus::Fail
    }
}

fn cargo_list_contains(bundle_root: &Path, member: &str, exact_name: &str) -> Result<bool> {
    let output = std::fs::read_to_string(bundle_root.join(member))
        .wrap_err_with(|| format!("read Cargo discovery member {member}"))?;
    Ok(output.lines().any(|line| {
        line.strip_prefix(exact_name)
            .and_then(|rest| rest.strip_prefix(':'))
            .is_some_and(|rest| rest.trim_start().starts_with("test"))
    }))
}

fn verify_p0_process(
    repo: &Path,
    bundle_root: &Path,
    receipt: &TestRunReceiptV1,
) -> Result<Vec<MemberDigestV1>> {
    ensure!(
        receipt.execution.succeeded(),
        "P0 process command did not succeed"
    );
    let root = bundle_root.join("p0-process");
    let feature_path = root.join("artifacts/outbe-chain-features.txt");
    let retained_feature_tree = std::fs::read(&feature_path)
        .wrap_err_with(|| format!("read {}", feature_path.display()))?;
    let exact_feature_tree = capture_outbe_chain_feature_tree(repo)?;
    ensure!(
        retained_feature_tree == exact_feature_tree,
        "retained P0 feature tree differs from the exact verifier command"
    );
    let cases = MetadosisP0Case::ALL
        .into_iter()
        .map(|case| MetadosisP0CaseInput {
            case,
            evidence_dir: root.join("cases").join(case.label()),
        })
        .collect::<Vec<_>>();
    let reconstructed = verify_metadosis_p0_parity(
        &receipt_source_sha(bundle_root)?,
        &exact_feature_tree,
        &root.join("artifacts/outbe-chain-debug"),
        &root.join("artifacts/outbe-chain-release"),
        &cases,
    )?;
    let mut reconstructed_bytes = serde_json::to_vec_pretty(&reconstructed)?;
    reconstructed_bytes.push(b'\n');
    let retained_path = root.join("metadosis-p0-parity.json");
    ensure!(
        std::fs::read(&retained_path)
            .wrap_err_with(|| format!("read {}", retained_path.display()))?
            == reconstructed_bytes,
        "retained P0 aggregate differs byte-for-byte from independent reconstruction"
    );

    let mut members = Vec::new();
    let mut scenario_records = 0_usize;
    let mut run_logs = 0_usize;
    for entry in WalkDir::new(&root).follow_links(false) {
        let entry = entry.wrap_err("walk retained P0 process artifacts")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(bundle_root)
            .wrap_err("P0 member escaped bundle")?;
        let relative = slash_path(relative)?;
        if !retained_p0_member(&relative) {
            continue;
        }
        if relative.starts_with("p0-process/cases/") && relative.ends_with(".json") {
            let bytes = std::fs::read(entry.path())?;
            if serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|value| {
                    value
                        .get("scenario")
                        .and_then(serde_json::Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .as_deref()
                == Some(METADOSIS_P0_SCENARIO)
            {
                scenario_records += 1;
            }
        }
        if relative.starts_with("p0-process/")
            && relative.ends_with(".log")
            && MetadosisP0Case::ALL
                .iter()
                .any(|case| relative == format!("p0-process/{}.log", case.label()))
        {
            run_logs += 1;
        }
        members.push(digest_member(bundle_root, &relative)?);
    }
    members.sort();
    ensure!(
        scenario_records == 6,
        "P0 bundle must retain six raw scenario records"
    );
    ensure!(run_logs == 6, "P0 bundle must retain six raw run logs");
    for required in [
        "p0-process/commands.log",
        "p0-process/verifier.log",
        "p0-process/metadosis-p0-parity.json",
        "p0-process/artifacts/outbe-chain-features.txt",
        "p0-process/artifacts/outbe-chain-debug",
        "p0-process/artifacts/outbe-chain-release",
    ] {
        ensure!(
            members.iter().any(|member| member.path == required),
            "P0 bundle is missing {required}"
        );
    }
    Ok(members)
}

fn receipt_source_sha(bundle_root: &Path) -> Result<String> {
    Ok(decode_receipt(&bundle_root.join(RUNNER_RECEIPT_MEMBER))?
        .source
        .sha)
}

fn decode_receipt(path: &Path) -> Result<MetadosisRunReceiptV1> {
    serde_json::from_slice(
        &std::fs::read(path).wrap_err_with(|| format!("read {}", path.display()))?,
    )
    .wrap_err_with(|| format!("decode Metadosis runner receipt {}", path.display()))
}

fn insert_member(
    members: &mut BTreeMap<String, MemberDigestV1>,
    member: MemberDigestV1,
) -> Result<()> {
    if let Some(existing) = members.insert(member.path.clone(), member.clone()) {
        ensure!(
            existing == member,
            "conflicting digest for evidence member {}",
            member.path
        );
    }
    Ok(())
}

fn slash_path(path: &Path) -> Result<String> {
    path.components()
        .map(|component| {
            component
                .as_os_str()
                .to_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| eyre::eyre!("P0 member path is not UTF-8"))
        })
        .collect::<Result<Vec<_>>>()
        .map(|parts| parts.join("/"))
}

#[cfg(test)]
mod tests;
