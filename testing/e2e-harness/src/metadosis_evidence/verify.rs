//! Derive and verify Metadosis evidence from runner-owned facts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use eyre::{ensure, Result, WrapErr};
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
    CommandReceiptV1, MetadosisRunReceiptV1, METADOSIS_NAMESPACE, METADOSIS_RUN_RECEIPT_KIND,
    METADOSIS_RUN_RECEIPT_SCHEMA_VERSION,
};
use crate::metadosis_p0::{
    canonical_final_fixture_sha256, capture_outbe_chain_feature_tree, verify_metadosis_p0_parity,
    MetadosisP0Case, MetadosisP0CaseInput, METADOSIS_P0_SCENARIO,
};
use crate::verification_ledger::{
    verify_domain_evidence, verify_member_digests, verify_source_checkout, AssertionStatus,
    DomainEvidenceV1, EvidenceTestV1, MemberDigestV1, TestDiscoveryV1, VerificationLedger,
    VERIFICATION_LEDGER_SCHEMA_VERSION,
};

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

        let (discovered, ignored) = if test_id == METADOSIS_P0_TEST_ID {
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
                "P0 process artifacts are incomplete or contain unverified members"
            );
            for member in required {
                refs.push(member.path.clone());
                insert_member(&mut all_members, member)?;
            }
            (true, false)
        } else if test_id == METADOSIS_FRESH_DEVNET_TEST_ID {
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
                "fresh-devnet process artifacts are incomplete or contain unverified members"
            );
            for member in required {
                refs.push(member.path.clone());
                insert_member(&mut all_members, member)?;
            }
            (true, false)
        } else {
            ensure!(
                receipt.process_artifacts.is_empty(),
                "{test_id} contains unexpected process artifacts"
            );
            let (normal_plan, ignored_plan) =
                plan.discovery.expect("non-P0 command has Cargo discovery");
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
                refs.push(member.path.clone());
                insert_member(&mut all_members, member.clone())?;
            }
            let name = definition.name.as_deref().unwrap_or(test_id);
            (
                observed.normal.succeeded()
                    && cargo_list_contains(bundle_root, &observed.normal.stdout.path, name)?,
                observed.ignored.succeeded()
                    && cargo_list_contains(bundle_root, &observed.ignored.stdout.path, name)?,
            )
        };

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

fn verify_fresh_devnet_process(
    bundle_root: &Path,
    receipt: &super::schema::TestRunReceiptV1,
) -> Result<Vec<MemberDigestV1>> {
    ensure!(
        receipt.execution.succeeded(),
        "fresh-devnet process command did not succeed"
    );
    let root = bundle_root.join("fresh-devnet");
    let mut members = Vec::new();
    let mut scenarios = Vec::new();
    for entry in WalkDir::new(&root).follow_links(false) {
        let entry = entry.wrap_err("walk retained fresh-devnet artifacts")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(bundle_root)
            .wrap_err("fresh-devnet member escaped bundle")?;
        let relative = slash_path(relative)?;
        if relative.starts_with("fresh-devnet/evidence/scenario-") && relative.ends_with(".json") {
            scenarios.push(relative.clone());
        }
        members.push(digest_member(bundle_root, &relative)?);
    }
    members.sort();
    ensure!(
        scenarios.len() == 1,
        "fresh-devnet process must retain exactly one scenario receipt"
    );
    let scenario: serde_json::Value =
        serde_json::from_slice(&std::fs::read(bundle_root.join(&scenarios[0]))?)
            .wrap_err("decode fresh-devnet scenario receipt")?;
    ensure!(
        scenario["feature"] == "Off-chain computation public path"
            && scenario["scenario"]
                == "A fresh Metadosis day runs from Create through terminal OCOMP"
            && scenario["result"] == "passed",
        "fresh-devnet scenario identity or verdict mismatch"
    );
    let expected_source_sha = receipt_source_sha(bundle_root)?;
    ensure!(
        scenario["source"]["sha"].as_str() == Some(expected_source_sha.as_str())
            && scenario["source"]["tracked_dirty"] == false
            && scenario["source"]["untracked_dirty"] == false,
        "fresh-devnet scenario source identity mismatch"
    );
    ensure!(
        scenario["environment"]["validators"] == 4
            && scenario["environment"]["tee"] == "gramine-direct"
            && scenario["environment"]["all"] == true,
        "fresh-devnet scenario did not use the required four-validator Linux topology"
    );
    let launch = &scenario["ocomp"]["topology"]["launch_identity"];
    ensure!(
        launch["classification"] == "measurement"
            && launch["activation_height"] == 1
            && launch["metadosis_storage_layout_hash"]
                == crate::world::ocomp::METADOSIS_STORAGE_LAYOUT_V1_HASH_HEX
            && launch["fork_install_hash"]
                .as_str()
                .is_some_and(|hash| hash.len() == 66)
            && launch["protocol_bundle_hash"]
                .as_str()
                .is_some_and(|hash| hash.len() == 66),
        "fresh-devnet launch identity is not Measurement@1 with the pinned Metadosis layout"
    );
    verify_fresh_artifacts(bundle_root, &scenario, &members)?;
    let processes = scenario["ocomp"]["topology"]["processes"]
        .as_array()
        .ok_or_else(|| eyre::eyre!("fresh-devnet scenario has no process topology"))?;
    for validator_index in 0..4_u64 {
        for role in ["supervisor", "snapshot_exporter", "worker"] {
            ensure!(
                processes.iter().any(|process| {
                    process["validator_index"].as_u64() == Some(validator_index)
                        && process["role"] == role
                        && process["stopped_at_millis"].is_null()
                }),
                "fresh-devnet validator {validator_index} has no live {role}"
            );
        }
    }
    let public = &scenario["ocomp"]["public_path"];
    let capacity = &public["capacity_public_path"];
    ensure!(
        public["atomic_quorum_apply_verified"] == true
            && public["activation"].is_object()
            && public["certified_generation"].is_object()
            && capacity["q_forming_receipt_success"] == true
            && capacity["canonical_import_verified"] == true
            && capacity["canonical_import_validator_count"] == 4
            && capacity["tribute_count"] == 257
            && capacity["nod_count"] == 257,
        "fresh-devnet scenario did not close the public READY/OCOMP/terminal path"
    );
    let lifecycle = &public["metadosis_fresh_lifecycle"];
    let worldwide_day = lifecycle["worldwide_day"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no WorldwideDay"))?;
    let forming_start = lifecycle["forming_start"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no forming_start"))?;
    let forming_end = lifecycle["forming_end"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no forming_end"))?;
    let lookback_end = lifecycle["lookback_end"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no lookback_end"))?;
    let offering_end = lifecycle["offering_end"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no offering_end"))?;
    let scheduled = lifecycle["scheduled_process_time"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no processing time"))?;
    ensure!(
        forming_end.checked_sub(forming_start) == Some(50 * 3_600)
            && lookback_end == forming_end
            && offering_end.checked_sub(lookback_end) == Some(48 * 3_600)
            && scheduled.checked_sub(offering_end) == Some(12 * 3_600),
        "fresh Metadosis lifecycle changed canonical WWD phase durations"
    );
    let initial_timestamp = lifecycle["initial_timestamp"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no initial timestamp"))?;
    ensure!(
        initial_timestamp >= forming_start
            && initial_timestamp < forming_end
            && lifecycle["initial_unix_time_offset_secs"].is_i64()
            && crate::world::localnet::ymd_utc(initial_timestamp)
                .parse::<u64>()
                .ok()
                == Some(worldwide_day)
            && initial_timestamp
                .checked_add(14 * 3_600)
                .map(crate::world::localnet::ymd_utc)
                .and_then(|day| day.parse::<u64>().ok())
                == Some(worldwide_day),
        "fresh Metadosis lifecycle did not begin inside FORMING on the block-1 UTC/UTC+14 WWD"
    );
    ensure!(
        lifecycle["genesis_hash"] == launch["genesis_hash"]
            && lifecycle["created_validator_count"] == 4
            && lifecycle["unknown_status_revert_validator_count"] == 4
            && lifecycle["offering_validator_count"] == 4
            && lifecycle["ready_validator_count"] == 4
            && lifecycle["completed_validator_count"] == 4,
        "fresh Metadosis lifecycle is not bound to one four-validator genesis"
    );
    let started = &lifecycle["started"];
    ensure!(
        started["worldwide_day"].as_u64() == Some(worldwide_day)
            && started["block_number"] == 1
            && started["forming_start"].as_u64() == Some(forming_start)
            && started["forming_end"].as_u64() == Some(forming_end)
            && started["lookback_end"].as_u64() == Some(lookback_end)
            && started["offering_end"].as_u64() == Some(offering_end)
            && started["scheduled_process_time"].as_u64() == Some(scheduled)
            && digest_like(&started["block_hash"]),
        "fresh Metadosis lifecycle has no canonical block-1 Create evidence"
    );
    let changes = lifecycle["status_changes"]
        .as_array()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no status changes"))?;
    let expected_edges = [(0_u64, 1_u64), (1, 2), (2, 3), (3, 4)];
    ensure!(
        changes.len() == expected_edges.len(),
        "fresh Metadosis lifecycle has an incomplete Advance edge set"
    );
    for (change, (old_status, new_status)) in changes.iter().zip(expected_edges) {
        ensure!(
            change["worldwide_day"].as_u64() == Some(worldwide_day)
                && change["old_status"].as_u64() == Some(old_status)
                && change["new_status"].as_u64() == Some(new_status)
                && change["block_number"]
                    .as_u64()
                    .is_some_and(|height| height >= 2)
                && digest_like(&change["block_hash"]),
            "fresh Metadosis lifecycle contains an invalid finalized Advance edge"
        );
    }
    let epochs = lifecycle["time_control_epochs"]
        .as_array()
        .ok_or_else(|| eyre::eyre!("fresh Metadosis lifecycle has no time-control history"))?;
    ensure!(
        epochs.len() == 2
            && epochs[0]["requested_timestamp"].as_u64() == forming_end.checked_add(1)
            && epochs[1]["requested_timestamp"].as_u64() == scheduled.checked_add(1),
        "fresh Metadosis lifecycle has incorrect logical-time boundaries"
    );
    let mut previous_after_height = 0;
    for epoch in epochs {
        ensure!(
            epoch["unix_time_offset_secs"].is_i64(),
            "fresh Metadosis time-control epoch has no signed offset"
        );
        let before = verified_time_control_points(&epoch["before_restart"])?;
        let after = verified_time_control_points(&epoch["after_restart"])?;
        ensure!(
            before[0] >= previous_after_height && after[0] > before[0],
            "fresh Metadosis time-control epoch breaks same-chain finality continuity"
        );
        previous_after_height = after[0];
    }
    ensure!(
        public["job_request"]["worldwide_day"].as_u64() == Some(worldwide_day)
            && public["activation"]["worldwide_day"].as_u64() == Some(worldwide_day)
            && digest_like(&public["activation"]["terminal_receipt_hash"])
            && public["certified_generation"]["worldwide_day"].as_u64() == Some(worldwide_day),
        "fresh Metadosis terminal evidence is not linked to the runtime-created WWD"
    );
    for required in [
        "fresh-devnet/commands.log",
        "fresh-devnet/run.log",
        scenarios[0].as_str(),
    ] {
        ensure!(
            members.iter().any(|member| member.path == required),
            "fresh-devnet bundle is missing {required}"
        );
    }
    Ok(members)
}

#[cfg(feature = "ocomp-integration")]
fn verify_fresh_artifacts(
    bundle_root: &Path,
    scenario: &serde_json::Value,
    members: &[MemberDigestV1],
) -> Result<()> {
    let exact_binaries = scenario["ocomp"]["exact_binaries"]
        .as_object()
        .ok_or_else(|| eyre::eyre!("fresh-devnet scenario has no exact binary digests"))?;
    let binaries = [
        ("outbe_chain", "outbe-chain"),
        ("outbe_ocomp", "outbe-ocomp"),
        ("outbe_cli", "outbe-cli"),
        ("outbe_keygen", "outbe-keygen"),
        ("outbe_e2e", "outbe-e2e"),
        ("outbe_tee_enclave", "outbe-tee-enclave"),
    ];
    ensure!(
        exact_binaries.len() == binaries.len(),
        "fresh-devnet scenario has an unexpected exact binary set"
    );
    for (identity, file_name) in binaries {
        let claim: MemberDigestV1 = serde_json::from_value(
            exact_binaries
                .get(identity)
                .cloned()
                .ok_or_else(|| eyre::eyre!("fresh-devnet scenario is missing {identity}"))?,
        )?;
        ensure!(
            Path::new(&claim.path)
                .file_name()
                .and_then(|name| name.to_str())
                == Some(file_name),
            "fresh-devnet scenario {identity} path does not identify {file_name}"
        );
        let relative = format!("fresh-devnet/artifacts/{file_name}");
        let retained = members
            .iter()
            .find(|member| member.path == relative)
            .ok_or_else(|| eyre::eyre!("fresh-devnet bundle is missing {relative}"))?;
        ensure!(
            claim.length == retained.length && claim.sha256 == retained.sha256,
            "fresh-devnet retained {file_name} differs from the executed binary"
        );
    }

    let genesis_relative = "fresh-devnet/artifacts/genesis.json";
    ensure!(
        members.iter().any(|member| member.path == genesis_relative),
        "fresh-devnet bundle is missing {genesis_relative}"
    );
    let genesis_path = bundle_root.join(genesis_relative);
    let genesis: serde_json::Value = serde_json::from_slice(&std::fs::read(&genesis_path)?)
        .wrap_err("decode retained fresh genesis")?;
    let chain_spec = reth_ethereum::cli::chainspec::chain_value_parser(
        genesis_path
            .to_str()
            .ok_or_else(|| eyre::eyre!("fresh genesis path is not UTF-8"))?,
    )?
    .as_ref()
    .clone()
    .map_header(OutbeHeader::new);
    let genesis_hash = chain_spec.genesis_hash();
    let launch = &scenario["ocomp"]["topology"]["launch_identity"];
    ensure!(
        launch["genesis_hash"].as_str() == Some(format!("{genesis_hash:#x}").as_str()),
        "fresh-devnet launch genesis hash is not derived from retained genesis bytes"
    );

    let manifest = &genesis["config"]["ocompForkInstallV1"];
    let canonical_hex = manifest["canonicalBytes"]
        .as_str()
        .and_then(|value| value.strip_prefix("0x"))
        .ok_or_else(|| eyre::eyre!("retained fresh genesis has no canonical OCOMP install"))?;
    let canonical_install = hex::decode(canonical_hex)?;
    let install_relative = "fresh-devnet/artifacts/fork-install-v1.ocb1";
    ensure!(
        std::fs::read(bundle_root.join(install_relative))? == canonical_install,
        "retained fresh fork install differs from genesis manifest bytes"
    );
    let limits = poc_schema_limits();
    let install = OcompForkInstallV1::decode_canonical(&canonical_install, &limits)?;
    install.validate_for_chain(chain_spec.chain().id(), genesis_hash, &limits)?;
    let install_hash = install.install_hash(&limits)?;
    let protocol_bundle_hash = install.protocol_bundle.protocol_bundle_hash(&limits)?;
    ensure!(
        install.classification == OcompForkInstallClassification::Measurement
            && install.activation_height == 1
            && launch["chain_id"].as_u64() == Some(install.request_profile.chain_id)
            && launch["fork_install_hash"].as_str() == Some(format!("{install_hash:#x}").as_str())
            && manifest["installHash"].as_str() == Some(format!("{install_hash:#x}").as_str())
            && launch["protocol_bundle_hash"].as_str()
                == Some(format!("{protocol_bundle_hash:#x}").as_str())
            && install.request_profile.genesis_hash == genesis_hash,
        "fresh-devnet launch identity is not derived from retained install/profile bytes"
    );
    ensure!(
        genesis["config"]["metadosisStorageLayoutV1"]["layoutHash"]
            == launch["metadosis_storage_layout_hash"],
        "fresh-devnet layout identity differs from retained genesis"
    );

    for (relative, expected) in [
        (
            "fresh-devnet/artifacts/protocol-bundle-v1.ocb1",
            install.protocol_bundle.encode_canonical(&limits)?,
        ),
        (
            "fresh-devnet/artifacts/capacity-profile-v1.ocb1",
            install
                .request_profile
                .capacity_profile
                .encode_canonical(&limits)?,
        ),
    ] {
        ensure!(
            members.iter().any(|member| member.path == relative)
                && std::fs::read(bundle_root.join(relative))? == expected,
            "fresh-devnet retained profile member {relative} is missing or mismatched"
        );
    }
    Ok(())
}

#[cfg(not(feature = "ocomp-integration"))]
fn verify_fresh_artifacts(
    _bundle_root: &Path,
    _scenario: &serde_json::Value,
    _members: &[MemberDigestV1],
) -> Result<()> {
    eyre::bail!("fresh-devnet artifact verification requires the ocomp-integration feature");
}

fn digest_like(value: &serde_json::Value) -> bool {
    value.as_str().is_some_and(|digest| {
        digest.len() == 66
            && digest.starts_with("0x")
            && hex::decode(&digest[2..])
                .is_ok_and(|bytes| bytes.len() == 32 && bytes.iter().any(|byte| *byte != 0))
    })
}

fn verified_time_control_points(value: &serde_json::Value) -> Result<[u64; 2]> {
    let points = value
        .as_array()
        .ok_or_else(|| eyre::eyre!("time-control finalized points are not an array"))?;
    ensure!(
        points.len() == 4,
        "time-control epoch must retain four validator points"
    );
    let first_height = points[0]["block_number"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("time-control point has no block number"))?;
    let first_timestamp = points[0]["block_timestamp"]
        .as_u64()
        .ok_or_else(|| eyre::eyre!("time-control point has no block timestamp"))?;
    let first_hash = &points[0]["block_hash"];
    ensure!(
        digest_like(first_hash),
        "time-control point has no canonical block hash"
    );
    let mut validators = BTreeSet::new();
    for point in points {
        let validator = point["validator_index"]
            .as_u64()
            .ok_or_else(|| eyre::eyre!("time-control point has no validator index"))?;
        ensure!(
            validator < 4 && validators.insert(validator),
            "time-control validator index is duplicate or outside the committee"
        );
        ensure!(
            point["block_number"].as_u64() == Some(first_height)
                && point["block_timestamp"].as_u64() == Some(first_timestamp)
                && point["block_hash"] == *first_hash,
            "time-control validators did not converge on one finalized point"
        );
    }
    Ok([first_height, first_timestamp])
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
    receipt: &super::schema::TestRunReceiptV1,
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
    let final_fixture_sha256 = canonical_final_fixture_sha256(repo)?;
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
        &final_fixture_sha256,
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
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::verification_ledger::{SourceIdentityV1, TestDiscoveryV1};

    #[test]
    fn hand_authored_pass_without_runner_receipt_is_rejected() {
        let evidence = DomainEvidenceV1 {
            schema_version: 1,
            namespace: METADOSIS_NAMESPACE.to_owned(),
            source: SourceIdentityV1 {
                sha: "a".repeat(40),
                tracked_dirty: false,
                untracked_dirty: false,
                cargo_lock_sha256: "b".repeat(64),
                rust_toolchain_sha256: "c".repeat(64),
                dependency_metadata_sha256: "d".repeat(64),
            },
            artifact_profile: "OUTBE_OCOMP_POC_DEVNET_FORK_V1".to_owned(),
            lane: "METADOSIS-LINUX".to_owned(),
            linux_image_digest: format!("sha256:{}", "e".repeat(64)),
            discovery: TestDiscoveryV1 {
                discovered: vec!["MET-FAKE-001".to_owned()],
                ..TestDiscoveryV1::default()
            },
            tests: vec![EvidenceTestV1 {
                test_id: "MET-FAKE-001".to_owned(),
                target: "fake".to_owned(),
                name: "fake".to_owned(),
                lane: "METADOSIS-LINUX".to_owned(),
                production_interface: "fake".to_owned(),
                substitutions: Vec::new(),
                status: AssertionStatus::Pass,
                evidence_member_refs: vec!["fake.log".to_owned()],
            }],
            members: vec![MemberDigestV1 {
                path: "fake.log".to_owned(),
                length: 0,
                sha256: "f".repeat(64),
            }],
        };
        let error = require_runner_derivation_member(&evidence).unwrap_err();
        assert!(error.to_string().contains("runner-receipt.json"));
    }

    #[test]
    fn status_is_derived_only_from_discovery_ignore_and_exit_facts() {
        assert_eq!(
            derive_status(true, false, true, false),
            AssertionStatus::Pass
        );
        assert_eq!(
            derive_status(true, false, false, false),
            AssertionStatus::Fail
        );
        assert_eq!(
            derive_status(true, false, false, true),
            AssertionStatus::InfraError
        );
        assert_eq!(
            derive_status(false, false, true, false),
            AssertionStatus::Missing
        );
        assert_eq!(
            derive_status(true, true, true, false),
            AssertionStatus::Skipped
        );
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn fresh_devnet_process_verifier_requires_measurement_one_layout_and_terminal_path() {
        let root = tempfile::tempdir().unwrap();
        let fresh = root.path().join("fresh-devnet");
        std::fs::create_dir_all(fresh.join("evidence")).unwrap();
        let artifacts = fresh.join("artifacts");
        std::fs::create_dir(&artifacts).unwrap();
        std::fs::write(fresh.join("commands.log"), b"exact fresh command\n").unwrap();
        std::fs::write(fresh.join("run.log"), b"scenario passed\n").unwrap();
        let source = SourceIdentityV1 {
            sha: "a".repeat(40),
            tracked_dirty: false,
            untracked_dirty: false,
            cargo_lock_sha256: "b".repeat(64),
            rust_toolchain_sha256: "c".repeat(64),
            dependency_metadata_sha256: "d".repeat(64),
        };
        let runner = MetadosisRunReceiptV1 {
            schema_version: METADOSIS_RUN_RECEIPT_SCHEMA_VERSION,
            kind: METADOSIS_RUN_RECEIPT_KIND.to_owned(),
            namespace: METADOSIS_NAMESPACE.to_owned(),
            source: source.clone(),
            artifact_profile: "OUTBE_OCOMP_POC_DEVNET_FORK_V1".to_owned(),
            lane: "METADOSIS-LINUX".to_owned(),
            linux_image_digest: format!("sha256:{}", "e".repeat(64)),
            launcher_nonce: "f".repeat(64),
            tests: Vec::new(),
        };
        std::fs::write(
            root.path().join(RUNNER_RECEIPT_MEMBER),
            serde_json::to_vec(&runner).unwrap(),
        )
        .unwrap();

        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut genesis: serde_json::Value =
            serde_json::from_slice(
                &std::fs::read(repo.join(
                    "testing/e2e-harness/fixtures/ocomp-final-v1/artifacts/genesis-final.json",
                ))
                .unwrap(),
            )
            .unwrap();
        let genesis_path = artifacts.join("genesis.json");
        std::fs::write(&genesis_path, serde_json::to_vec_pretty(&genesis).unwrap()).unwrap();
        let base_spec =
            reth_ethereum::cli::chainspec::chain_value_parser(genesis_path.to_str().unwrap())
                .unwrap()
                .as_ref()
                .clone()
                .map_header(OutbeHeader::new);
        let chain_id = base_spec.chain().id();
        let genesis_hash_value = base_spec.genesis_hash();
        let limits = poc_schema_limits();
        let install = ForkInstallScenario::measurement_at(1, chain_id, genesis_hash_value)
            .unwrap()
            .into_install();
        install
            .validate_for_chain(chain_id, genesis_hash_value, &limits)
            .unwrap();
        let canonical_install = install.encode_canonical(&limits).unwrap();
        let install_hash = install.install_hash(&limits).unwrap();
        let protocol_bundle_hash = install
            .protocol_bundle
            .protocol_bundle_hash(&limits)
            .unwrap();
        let config = genesis["config"].as_object_mut().unwrap();
        config.insert(
            "ocompForkInstallV1".to_owned(),
            serde_json::json!({
                "canonicalBytes": format!("0x{}", hex::encode(&canonical_install)),
                "installHash": install_hash,
            }),
        );
        config.insert(
            "metadosisStorageLayoutV1".to_owned(),
            serde_json::json!({
                "layoutHash": crate::world::ocomp::METADOSIS_STORAGE_LAYOUT_V1_HASH_HEX,
            }),
        );
        std::fs::write(&genesis_path, serde_json::to_vec_pretty(&genesis).unwrap()).unwrap();
        std::fs::write(artifacts.join("fork-install-v1.ocb1"), &canonical_install).unwrap();
        std::fs::write(
            artifacts.join("protocol-bundle-v1.ocb1"),
            install.protocol_bundle.encode_canonical(&limits).unwrap(),
        )
        .unwrap();
        std::fs::write(
            artifacts.join("capacity-profile-v1.ocb1"),
            install
                .request_profile
                .capacity_profile
                .encode_canonical(&limits)
                .unwrap(),
        )
        .unwrap();
        let mut exact_binaries = serde_json::Map::new();
        for (identity, file_name) in [
            ("outbe_chain", "outbe-chain"),
            ("outbe_ocomp", "outbe-ocomp"),
            ("outbe_cli", "outbe-cli"),
            ("outbe_keygen", "outbe-keygen"),
            ("outbe_e2e", "outbe-e2e"),
            ("outbe_tee_enclave", "outbe-tee-enclave"),
        ] {
            let binary = artifacts.join(file_name);
            std::fs::write(&binary, format!("exact {file_name} bytes")).unwrap();
            exact_binaries.insert(
                identity.to_owned(),
                serde_json::to_value(crate::ocomp_evidence::hash_file(&binary).unwrap()).unwrap(),
            );
        }
        let mut processes = Vec::new();
        for validator_index in 0..4 {
            for role in ["supervisor", "snapshot_exporter", "worker"] {
                processes.push(serde_json::json!({
                    "validator_index": validator_index,
                    "role": role,
                    "stopped_at_millis": null
                }));
            }
        }
        let finalized_points = |block_number: u64, block_timestamp: u64, hash_byte: u8| {
            (0..4)
                .map(|validator_index| {
                    serde_json::json!({
                        "validator_index": validator_index,
                        "block_number": block_number,
                        "block_hash": format!("0x{}", format!("{hash_byte:02x}").repeat(32)),
                        "block_timestamp": block_timestamp
                    })
                })
                .collect::<Vec<_>>()
        };
        let forming_start = 1_785_319_200_u64;
        let initial_timestamp = forming_start + 15 * 3_600;
        let forming_end = forming_start + 50 * 3_600;
        let offering_end = forming_end + 48 * 3_600;
        let scheduled_process_time = offering_end + 12 * 3_600;
        let first_target = forming_end + 1;
        let second_target = scheduled_process_time + 1;
        let before_first_restart = finalized_points(1, initial_timestamp, 0x44);
        let after_first_restart = finalized_points(2, first_target, 0x55);
        let before_second_restart = finalized_points(2, first_target, 0x55);
        let after_second_restart = finalized_points(3, second_target, 0x66);
        let worldwide_day = 20_260_730_u64;
        let genesis_hash = format!("{genesis_hash_value:#x}");
        let scenario_path = fresh.join("evidence/scenario-001.json");
        let mut scenario = serde_json::json!({
            "source": {
                "sha": source.sha,
                "tracked_dirty": false,
                "untracked_dirty": false
            },
            "feature": "Off-chain computation public path",
            "scenario": "A fresh Metadosis day runs from Create through terminal OCOMP",
            "result": "passed",
            "environment": {
                "validators": 4,
                "tee": "gramine-direct",
                "all": true
            },
            "ocomp": {
                "exact_binaries": exact_binaries,
                "topology": {
                    "launch_identity": {
                        "classification": "measurement",
                        "activation_height": 1,
                        "chain_id": chain_id,
                        "genesis_hash": genesis_hash,
                        "metadosis_storage_layout_hash":
                            crate::world::ocomp::METADOSIS_STORAGE_LAYOUT_V1_HASH_HEX,
                        "fork_install_hash": format!("{install_hash:#x}"),
                        "protocol_bundle_hash": format!("{protocol_bundle_hash:#x}")
                    },
                    "processes": processes
                },
                "public_path": {
                    "atomic_quorum_apply_verified": true,
                    "job_request": {
                        "worldwide_day": worldwide_day
                    },
                    "activation": {
                        "worldwide_day": worldwide_day,
                        "terminal_receipt_hash": format!("0x{}", "77".repeat(32))
                    },
                    "certified_generation": {
                        "worldwide_day": worldwide_day
                    },
                    "capacity_public_path": {
                        "q_forming_receipt_success": true,
                        "canonical_import_verified": true,
                        "canonical_import_validator_count": 4,
                        "tribute_count": 257,
                        "nod_count": 257
                    },
                    "metadosis_fresh_lifecycle": {
                        "worldwide_day": worldwide_day,
                        "genesis_hash": genesis_hash,
                        "initial_timestamp": initial_timestamp,
                        "initial_unix_time_offset_secs": -10,
                        "forming_start": forming_start,
                        "forming_end": forming_end,
                        "lookback_end": forming_end,
                        "offering_end": offering_end,
                        "scheduled_process_time": scheduled_process_time,
                        "started": {
                            "worldwide_day": worldwide_day,
                            "forming_start": forming_start,
                            "forming_end": forming_end,
                            "lookback_end": forming_end,
                            "offering_end": offering_end,
                            "scheduled_process_time": scheduled_process_time,
                            "block_number": 1,
                            "block_hash": format!("0x{}", "44".repeat(32))
                        },
                        "status_changes": [
                            {
                                "worldwide_day": worldwide_day,
                                "old_status": 0,
                                "new_status": 1,
                                "block_number": 2,
                                "block_hash": format!("0x{}", "55".repeat(32))
                            },
                            {
                                "worldwide_day": worldwide_day,
                                "old_status": 1,
                                "new_status": 2,
                                "block_number": 2,
                                "block_hash": format!("0x{}", "55".repeat(32))
                            },
                            {
                                "worldwide_day": worldwide_day,
                                "old_status": 2,
                                "new_status": 3,
                                "block_number": 3,
                                "block_hash": format!("0x{}", "66".repeat(32))
                            },
                            {
                                "worldwide_day": worldwide_day,
                                "old_status": 3,
                                "new_status": 4,
                                "block_number": 3,
                                "block_hash": format!("0x{}", "66".repeat(32))
                            }
                        ],
                        "time_control_epochs": [
                            {
                                "requested_timestamp": first_target,
                                "unix_time_offset_secs": 10,
                                "before_restart": before_first_restart,
                                "after_restart": after_first_restart
                            },
                            {
                                "requested_timestamp": second_target,
                                "unix_time_offset_secs": 20,
                                "before_restart": before_second_restart,
                                "after_restart": after_second_restart
                            }
                        ],
                        "created_validator_count": 4,
                        "unknown_status_revert_validator_count": 4,
                        "offering_validator_count": 4,
                        "ready_validator_count": 4,
                        "completed_validator_count": 4
                    }
                }
            }
        });
        std::fs::write(&scenario_path, serde_json::to_vec(&scenario).unwrap()).unwrap();
        let dummy_member = MemberDigestV1 {
            path: "unused".to_owned(),
            length: 1,
            sha256: "f".repeat(64),
        };
        let receipt = crate::metadosis_evidence::schema::TestRunReceiptV1 {
            test_id: METADOSIS_FRESH_DEVNET_TEST_ID.to_owned(),
            discovery: None,
            execution: CommandReceiptV1 {
                argv: vec![
                    "outbe-metadosis-evidence".to_owned(),
                    "fresh-devnet".to_owned(),
                    "--output".to_owned(),
                    root.path().join("fresh-devnet").display().to_string(),
                ],
                attempt: 1,
                started_at_unix_ms: 1,
                finished_at_unix_ms: 2,
                exit_code: Some(0),
                stdout: dummy_member.clone(),
                stderr: dummy_member,
            },
            process_artifacts: Vec::new(),
        };
        verify_fresh_devnet_process(root.path(), &receipt).unwrap();

        let retained_chain = std::fs::read(artifacts.join("outbe-chain")).unwrap();
        std::fs::write(artifacts.join("outbe-chain"), b"tampered binary").unwrap();
        assert!(verify_fresh_devnet_process(root.path(), &receipt).is_err());
        std::fs::write(artifacts.join("outbe-chain"), retained_chain).unwrap();

        let retained_bundle = std::fs::read(artifacts.join("protocol-bundle-v1.ocb1")).unwrap();
        std::fs::remove_file(artifacts.join("protocol-bundle-v1.ocb1")).unwrap();
        assert!(verify_fresh_devnet_process(root.path(), &receipt).is_err());
        std::fs::write(artifacts.join("protocol-bundle-v1.ocb1"), retained_bundle).unwrap();

        scenario["ocomp"]["public_path"]["metadosis_fresh_lifecycle"]
            ["unknown_status_revert_validator_count"] = serde_json::json!(3);
        std::fs::write(&scenario_path, serde_json::to_vec(&scenario).unwrap()).unwrap();
        assert!(verify_fresh_devnet_process(root.path(), &receipt).is_err());

        scenario["ocomp"]["public_path"]["metadosis_fresh_lifecycle"]
            ["unknown_status_revert_validator_count"] = serde_json::json!(4);
        scenario["ocomp"]["public_path"]["metadosis_fresh_lifecycle"]["status_changes"][0]
            ["new_status"] = serde_json::json!(2);
        std::fs::write(&scenario_path, serde_json::to_vec(&scenario).unwrap()).unwrap();
        assert!(verify_fresh_devnet_process(root.path(), &receipt).is_err());
    }

    #[test]
    fn checked_in_metadosis_pack_has_one_fixed_command_per_test() {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let ledger =
            VerificationLedger::parse(&repo.join("outbe-plan/verification-ledger.yaml")).unwrap();
        let pack = ledger.domain_pack(METADOSIS_NAMESPACE).unwrap();
        let bundle = Path::new("/evidence/metadosis");
        let plans = pack
            .tests
            .iter()
            .map(|(test_id, definition)| {
                (
                    test_id.clone(),
                    command_plan(test_id, definition, bundle).unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(plans.len(), pack.tests.len());
        let p0 = plans.get(METADOSIS_P0_TEST_ID).unwrap();
        assert!(p0.discovery.is_none());
        assert_eq!(
            p0.execution,
            [
                "outbe-metadosis-evidence",
                "p0-parity",
                "--output",
                "/evidence/metadosis/p0-process",
            ]
        );
        let fresh = plans.get(METADOSIS_FRESH_DEVNET_TEST_ID).unwrap();
        assert!(fresh.discovery.is_none());
        assert_eq!(
            fresh.execution,
            [
                "outbe-metadosis-evidence",
                "fresh-devnet",
                "--output",
                "/evidence/metadosis/fresh-devnet",
            ]
        );
        assert!(plans
            .iter()
            .filter(|(id, _)| {
                !matches!(
                    id.as_str(),
                    METADOSIS_P0_TEST_ID | METADOSIS_FRESH_DEVNET_TEST_ID
                )
            })
            .all(|(_, plan)| plan.discovery.is_some() && plan.execution[0] == "cargo"));
        let test_utils_privacy = plans.get("MET-AUTH-003").unwrap();
        assert_eq!(
            test_utils_privacy.execution,
            [
                "cargo",
                "test",
                "--locked",
                "-p",
                "outbe-metadosis",
                "--features",
                "test-utils",
                "--test",
                "external_mutation_surface",
                "test_utils_keeps_the_raw_fixture_kernel_private",
                "--",
                "--exact",
                "--nocapture",
            ]
        );
    }
}
