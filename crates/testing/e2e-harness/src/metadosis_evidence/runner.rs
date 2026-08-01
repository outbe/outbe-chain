//! Exact, non-retrying Metadosis Linux command runner.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use eyre::{bail, ensure, Result, WrapErr};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use super::schema::{
    CargoDiscoveryReceiptV1, CommandReceiptV1, MetadosisRunReceiptV1, TestRunReceiptV1,
    METADOSIS_NAMESPACE, METADOSIS_RUN_RECEIPT_KIND, METADOSIS_RUN_RECEIPT_SCHEMA_VERSION,
};
use crate::verification_ledger::{
    capture_source_identity, MemberDigestV1, TestDefinitionV1, VerificationLedger,
};

pub const METADOSIS_P0_TEST_ID: &str = "MET-ENV-001";
pub const METADOSIS_P0_TARGET: &str = "outbe-e2e-harness";
pub const METADOSIS_P0_NAME: &str = "metadosis_process::p0_parity";
pub const METADOSIS_FRESH_DEVNET_TEST_ID: &str = "MET-GEN-005";
pub const METADOSIS_FRESH_DEVNET_TARGET: &str = "outbe-e2e-harness";
pub const METADOSIS_FRESH_DEVNET_NAME: &str = "metadosis_process::fresh_devnet";
const METADOSIS_EVIDENCE_BINARY: &str = "outbe-metadosis-evidence";
pub const RUNNER_RECEIPT_MEMBER: &str = "runner-receipt.json";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandPlan {
    pub execution: Vec<String>,
    pub discovery: Option<(Vec<String>, Vec<String>)>,
}

/// The domain adapter owns the small set of Cargo target shapes used by this
/// pack. No command string or PASS status is accepted from evidence input.
pub fn command_plan(
    test_id: &str,
    definition: &TestDefinitionV1,
    bundle_root: &Path,
) -> Result<CommandPlan> {
    let name = definition.name.as_deref().unwrap_or(test_id);
    if test_id == METADOSIS_P0_TEST_ID {
        ensure!(
            definition.target == METADOSIS_P0_TARGET && name == METADOSIS_P0_NAME,
            "MET-ENV-001 must remain the exact P0 process target"
        );
        return Ok(CommandPlan {
            execution: vec![
                METADOSIS_EVIDENCE_BINARY.to_owned(),
                "p0-parity".to_owned(),
                "--output".to_owned(),
                bundle_root.join("p0-process").display().to_string(),
            ],
            discovery: None,
        });
    }
    if test_id == METADOSIS_FRESH_DEVNET_TEST_ID {
        ensure!(
            definition.target == METADOSIS_FRESH_DEVNET_TARGET
                && name == METADOSIS_FRESH_DEVNET_NAME,
            "MET-GEN-005 must remain the exact fresh-devnet process target"
        );
        return Ok(CommandPlan {
            execution: vec![
                METADOSIS_EVIDENCE_BINARY.to_owned(),
                "fresh-devnet".to_owned(),
                "--output".to_owned(),
                bundle_root.join("fresh-devnet").display().to_string(),
            ],
            discovery: None,
        });
    }

    let (target_args, cargo_features): (Vec<&str>, Vec<&str>) = match test_id {
        "MET-AUTH-001" => (vec!["--test", "external_mutation_surface"], Vec::new()),
        "MET-AUTH-003" => (
            vec!["--test", "external_mutation_surface"],
            vec!["--features", "test-utils"],
        ),
        "MET-GEN-004" | "MET-GEN-006" | "MET-EVIDENCE-012" => {
            (vec!["--lib"], vec!["--features", "ocomp-integration"])
        }
        "MET-MODEL-004" | "MET-MODEL-005" => {
            (vec!["--test", "ocomp_request_lifecycle"], Vec::new())
        }
        "MET-EVIDENCE-002" | "MET-EVIDENCE-003" | "MET-EVIDENCE-004" | "MET-EVIDENCE-005"
        | "MET-EVIDENCE-006" | "MET-EVIDENCE-007" | "MET-EVIDENCE-008" | "MET-EVIDENCE-009"
        | "MET-EVIDENCE-010" | "MET-EVIDENCE-011" | "MET-EVIDENCE-013" => {
            (vec!["--test", "verification_ledger"], Vec::new())
        }
        _ => {
            ensure!(
                matches!(
                    definition.target.as_str(),
                    "outbe-metadosis"
                        | "outbe-cycle"
                        | "outbe-desis"
                        | "outbe-node"
                        | "outbe-e2e-harness"
                ),
                "Metadosis pack contains unsupported Cargo target {}",
                definition.target
            );
            let features = if definition.target == "outbe-metadosis" {
                vec!["--features", "test-utils"]
            } else {
                Vec::new()
            };
            (vec!["--lib"], features)
        }
    };

    let mut prefix = vec![
        "cargo".to_owned(),
        "test".to_owned(),
        "--locked".to_owned(),
        "-p".to_owned(),
        definition.target.clone(),
    ];
    prefix.extend(cargo_features.into_iter().map(ToOwned::to_owned));
    prefix.extend(target_args.into_iter().map(ToOwned::to_owned));
    prefix.push(name.to_owned());

    let mut execution = prefix.clone();
    execution.extend(["--", "--exact", "--nocapture"].map(ToOwned::to_owned));
    let mut normal = prefix.clone();
    normal.extend(["--", "--exact", "--list"].map(ToOwned::to_owned));
    let mut ignored = prefix;
    ignored.extend(["--", "--exact", "--ignored", "--list"].map(ToOwned::to_owned));
    Ok(CommandPlan {
        execution,
        discovery: Some((normal, ignored)),
    })
}

/// Execute the entire checked-in Metadosis pack once. The output directory is
/// new and outside the repository so source identity remains stable.
pub fn run(
    repo: &Path,
    index_path: &Path,
    output: &Path,
    linux_image_digest: &str,
    launcher_nonce: &str,
) -> Result<PathBuf> {
    validate_linux_image_digest(linux_image_digest)?;
    validate_lower_hex_32(launcher_nonce, "launcher nonce")?;
    let repo = repo.canonicalize().wrap_err("canonicalize repository")?;
    ensure!(!output.exists(), "evidence output already exists");
    let output_parent = output
        .parent()
        .ok_or_else(|| eyre::eyre!("evidence output has no parent"))?
        .canonicalize()
        .wrap_err("canonicalize evidence output parent")?;
    let output = output_parent.join(
        output
            .file_name()
            .ok_or_else(|| eyre::eyre!("evidence output has no file name"))?,
    );
    ensure!(
        !output.starts_with(&repo),
        "Metadosis evidence output must be outside the source checkout"
    );

    let ledger = VerificationLedger::parse(index_path)?;
    let pack = ledger.domain_pack(METADOSIS_NAMESPACE)?;
    let source = capture_source_identity(&repo)?;
    let identity = &pack.verification.evidence_identity;
    ensure!(
        !identity.require_clean_tracked_tree || !source.tracked_dirty,
        "tracked source tree is dirty"
    );
    ensure!(
        !identity.require_clean_untracked_tree || !source.untracked_dirty,
        "untracked source tree is dirty"
    );

    std::fs::create_dir(&output)
        .wrap_err_with(|| format!("create evidence output {}", output.display()))?;
    let mut tests = Vec::with_capacity(pack.tests.len());
    let mut any_failed = false;
    for (test_id, definition) in &pack.tests {
        let plan = command_plan(test_id, definition, &output)?;
        let discovery = if let Some((normal, ignored)) = plan.discovery {
            let normal = execute(
                &repo,
                &output,
                &format!("raw/{test_id}/discovery-normal"),
                &normal,
            )?;
            let ignored = execute(
                &repo,
                &output,
                &format!("raw/{test_id}/discovery-ignored"),
                &ignored,
            )?;
            any_failed |= !normal.succeeded() || !ignored.succeeded();
            Some(CargoDiscoveryReceiptV1 { normal, ignored })
        } else {
            None
        };
        let execution = execute(
            &repo,
            &output,
            &format!("raw/{test_id}/execution"),
            &plan.execution,
        )?;
        any_failed |= !execution.succeeded();
        let process_artifacts = if execution.succeeded() {
            match test_id.as_str() {
                METADOSIS_P0_TEST_ID => collect_process_artifacts(&output)?,
                METADOSIS_FRESH_DEVNET_TEST_ID => collect_tree_artifacts(&output, "fresh-devnet")?,
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        tests.push(TestRunReceiptV1 {
            test_id: test_id.clone(),
            discovery,
            execution,
            process_artifacts,
        });
    }

    let receipt = MetadosisRunReceiptV1 {
        schema_version: METADOSIS_RUN_RECEIPT_SCHEMA_VERSION,
        kind: METADOSIS_RUN_RECEIPT_KIND.to_owned(),
        namespace: METADOSIS_NAMESPACE.to_owned(),
        source,
        artifact_profile: identity.artifact_profile.clone(),
        lane: identity.linux_lane.clone(),
        linux_image_digest: linux_image_digest.to_owned(),
        launcher_nonce: launcher_nonce.to_owned(),
        tests,
    };
    let receipt_path = output.join(RUNNER_RECEIPT_MEMBER);
    write_new_json(&receipt_path, &receipt)?;
    if any_failed {
        bail!(
            "one or more Metadosis evidence commands failed; retained receipt {}",
            receipt_path.display()
        );
    }
    Ok(receipt_path)
}

fn execute(
    repo: &Path,
    bundle_root: &Path,
    member_prefix: &str,
    argv: &[String],
) -> Result<CommandReceiptV1> {
    ensure!(!argv.is_empty(), "empty evidence command");
    let started_at_unix_ms = unix_ms()?;
    let program = if argv[0] == METADOSIS_EVIDENCE_BINARY {
        std::env::current_exe().wrap_err("resolve Metadosis evidence runner binary")?
    } else {
        PathBuf::from(&argv[0])
    };
    let output = Command::new(program)
        .args(&argv[1..])
        .current_dir(repo)
        .output()
        .wrap_err_with(|| format!("execute {}", argv.join(" ")))?;
    let finished_at_unix_ms = unix_ms()?;
    let stdout_path = format!("{member_prefix}.stdout");
    let stderr_path = format!("{member_prefix}.stderr");
    write_new(&bundle_root.join(&stdout_path), &output.stdout)?;
    write_new(&bundle_root.join(&stderr_path), &output.stderr)?;
    Ok(CommandReceiptV1 {
        argv: argv.to_vec(),
        attempt: 1,
        started_at_unix_ms,
        finished_at_unix_ms,
        exit_code: output.status.code(),
        stdout: digest_member(bundle_root, &stdout_path)?,
        stderr: digest_member(bundle_root, &stderr_path)?,
    })
}

fn collect_process_artifacts(bundle_root: &Path) -> Result<Vec<MemberDigestV1>> {
    let process_root = bundle_root.join("p0-process");
    ensure!(
        process_root.is_dir(),
        "successful P0 command produced no process directory"
    );
    let mut members = Vec::new();
    for entry in WalkDir::new(&process_root).follow_links(false) {
        let entry = entry.wrap_err("walk P0 process evidence")?;
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
        members.push(digest_member(bundle_root, &relative)?);
    }
    members.sort();
    let unique = members
        .iter()
        .map(|member| member.path.as_str())
        .collect::<BTreeSet<_>>();
    ensure!(unique.len() == members.len(), "duplicate P0 process member");
    Ok(members)
}

fn collect_tree_artifacts(bundle_root: &Path, relative_root: &str) -> Result<Vec<MemberDigestV1>> {
    let root = bundle_root.join(relative_root);
    ensure!(
        root.is_dir(),
        "successful process command produced no {relative_root} directory"
    );
    let mut members = Vec::new();
    for entry in WalkDir::new(&root).follow_links(false) {
        let entry = entry.wrap_err("walk process evidence")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(bundle_root)
            .wrap_err("process evidence member escaped bundle")?;
        members.push(digest_member(bundle_root, &slash_path(relative)?)?);
    }
    members.sort();
    ensure!(!members.is_empty(), "process evidence directory is empty");
    let unique = members
        .iter()
        .map(|member| member.path.as_str())
        .collect::<BTreeSet<_>>();
    ensure!(
        unique.len() == members.len(),
        "duplicate process evidence member"
    );
    Ok(members)
}

pub(crate) fn retained_p0_member(relative: &str) -> bool {
    relative.starts_with("p0-process/cases/")
        || matches!(
            relative,
            "p0-process/commands.log"
                | "p0-process/build.log"
                | "p0-process/verifier.log"
                | "p0-process/metadosis-p0-parity.json"
                | "p0-process/artifacts/outbe-chain-features.txt"
                | "p0-process/artifacts/outbe-chain-debug"
                | "p0-process/artifacts/outbe-chain-release"
                | "p0-process/debug_unset.log"
                | "p0-process/debug_named.log"
                | "p0-process/debug_arbitrary.log"
                | "p0-process/release_unset.log"
                | "p0-process/release_named.log"
                | "p0-process/release_arbitrary.log"
        )
}

pub(crate) fn digest_member(bundle_root: &Path, relative: &str) -> Result<MemberDigestV1> {
    let bytes = std::fs::read(bundle_root.join(relative))
        .wrap_err_with(|| format!("read evidence member {relative}"))?;
    Ok(MemberDigestV1 {
        path: relative.to_owned(),
        length: u64::try_from(bytes.len()).wrap_err("member length does not fit u64")?,
        sha256: hex::encode(Sha256::digest(&bytes)),
    })
}

pub(crate) fn write_new_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_new(path, &bytes)
}

pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("evidence member has no parent"))?;
    std::fs::create_dir_all(parent)
        .wrap_err_with(|| format!("create evidence parent {}", parent.display()))?;
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    use std::io::Write as _;
    let mut file = options
        .open(path)
        .wrap_err_with(|| format!("create immutable evidence member {}", path.display()))?;
    file.write_all(bytes)
        .wrap_err_with(|| format!("write evidence member {}", path.display()))?;
    file.sync_all()
        .wrap_err_with(|| format!("sync evidence member {}", path.display()))
}

fn unix_ms() -> Result<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .wrap_err("system clock is before Unix epoch")?
        .as_millis();
    u64::try_from(millis).wrap_err("Unix millisecond timestamp does not fit u64")
}

fn validate_linux_image_digest(value: &str) -> Result<()> {
    let digest = value
        .strip_prefix("sha256:")
        .ok_or_else(|| eyre::eyre!("Linux image digest must start with sha256:"))?;
    ensure!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "Linux image digest is not exact lowercase SHA-256"
    );
    Ok(())
}

fn validate_lower_hex_32(value: &str, what: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{what} is not exact lowercase 32-byte hex"
    );
    Ok(())
}

fn slash_path(path: &Path) -> Result<String> {
    let components = path
        .components()
        .map(|component| {
            component
                .as_os_str()
                .to_str()
                .ok_or_else(|| eyre::eyre!("evidence path is not UTF-8"))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(components.join("/"))
}
