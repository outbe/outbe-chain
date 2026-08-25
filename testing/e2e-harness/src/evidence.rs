use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cucumber::event::ScenarioFinished;
use cucumber::gherkin::{Feature, Scenario};
use eyre::{Result, WrapErr};
use serde_json::json;

use crate::env::Environment;
use crate::ocomp_evidence::{hash_file, publish_member};
use crate::world::localnet::LogAudit;
use crate::world::ocomp::OcompScenarioTopologyV1;
use crate::world::price_oracle::PriceOracleEvidenceV1;
use crate::world::state::{OcompPublicScenarioEvidenceV1, RadicleScenarioEvidenceV1};

pub(crate) struct ScenarioEvidence<'a> {
    pub env: &'a Environment,
    pub feature: &'a Feature,
    pub scenario: &'a Scenario,
    pub event: &'a ScenarioFinished,
    pub scenario_id: usize,
    pub scenario_dir: &'a Path,
    pub elapsed: Duration,
    pub audit: &'a LogAudit,
    pub gramine_image_id: Option<&'a str>,
    pub ocomp: &'a OcompScenarioTopologyV1,
    pub ocomp_public: &'a OcompPublicScenarioEvidenceV1,
    pub price_oracle: &'a PriceOracleEvidenceV1,
    pub radicle: &'a RadicleScenarioEvidenceV1,
}

pub(crate) fn write_scenario(input: ScenarioEvidence<'_>) -> Result<()> {
    input
        .ocomp
        .validate()
        .wrap_err("validate OCOMP scenario topology evidence")?;
    let evidence_dir = input
        .env
        .evidence_dir
        .as_ref()
        .expect("run() resolves the evidence directory");
    let (sha, tracked_dirty, untracked_dirty) = git_identity(&input.env.repo);
    let exact_ocomp_binaries = if input.ocomp.launch_identity.is_some() {
        let current_exe = std::env::current_exe().wrap_err("resolve exact outbe-e2e binary")?;
        let mut binaries = serde_json::Map::new();
        for (name, path) in [
            ("outbe_chain", input.env.chain_bin.as_path()),
            ("outbe_ocomp", input.env.ocomp_bin.as_path()),
            ("outbe_feeder", input.env.feeder_bin.as_path()),
            ("outbe_cli", input.env.cli_bin.as_path()),
            ("outbe_keygen", input.env.keygen_bin.as_path()),
            ("outbe_e2e", current_exe.as_path()),
        ] {
            binaries.insert(
                name.to_owned(),
                serde_json::to_value(
                    hash_file(path).wrap_err_with(|| format!("hash exact {name} binary"))?,
                )?,
            );
        }
        let enclave_name = if input.env.tee_mode.uses_mock_binary() {
            "outbe_tee_enclave_mock"
        } else {
            "outbe_tee_enclave"
        };
        binaries.insert(
            enclave_name.to_owned(),
            serde_json::to_value(
                hash_file(input.env.selected_enclave_bin())
                    .wrap_err_with(|| format!("hash exact {enclave_name} binary"))?,
            )?,
        );
        Some(serde_json::Value::Object(binaries))
    } else {
        None
    };
    let exact_radicle_binaries = if input.radicle.validator_node_ids.is_empty() {
        None
    } else {
        let heartwood_target = input
            .env
            .repo
            .parent()
            .unwrap_or(&input.env.repo)
            .join("outbe-heartwood/target/release");
        let mut binaries = serde_json::Map::new();
        for (name, path) in [
            (
                "outbe_radicle",
                input.env.repo.join("target/release/outbe-radicle"),
            ),
            ("rad", heartwood_target.join("rad")),
            ("git_remote_rad", heartwood_target.join("git-remote-rad")),
        ] {
            binaries.insert(
                name.to_owned(),
                serde_json::to_value(
                    hash_file(&path).wrap_err_with(|| format!("hash exact {name} binary"))?,
                )?,
            );
        }
        Some(serde_json::Value::Object(binaries))
    };
    let metadosis_p0 = input
        .env
        .metadosis_p0
        .as_ref()
        .map(|environment| {
            Ok::<_, eyre::Report>(json!({
                "environment": environment,
                "genesis": hash_file(&input.scenario_dir.join("genesis.json"))
                    .wrap_err("hash Metadosis P0 canonical genesis")?,
            }))
        })
        .transpose()?;
    let document = json!({
        "schema_version": 1,
        "recorded_at_unix_ms": unix_millis(),
        "source": {
            "sha": sha,
            "dirty": tracked_dirty.zip(untracked_dirty).map(|(tracked, untracked)| tracked || untracked),
            "tracked_dirty": tracked_dirty,
            "untracked_dirty": untracked_dirty,
        },
        "invocation": std::env::args().collect::<Vec<_>>(),
        "feature": input.feature.name,
        "scenario": input.scenario.name,
        "scenario_id": input.scenario_id,
        "result": event_name(input.event),
        "duration_ms": input.elapsed.as_millis(),
        "environment": {
            "validators": input.env.validators,
            "tee": input.env.tee_mode.evidence_name(),
            "sudo": input.env.sudo,
            "all": input.env.all,
            "gramine_image_id": input.gramine_image_id,
        },
        "scenario_data_dir": input.scenario_dir,
        "metadosis_p0": metadosis_p0,
        "log_audit": input.audit.json(),
        "ocomp": {
            "exact_binaries": exact_ocomp_binaries,
            "topology": input.ocomp,
            "public_path": input.ocomp_public,
        },
        "price_oracle": input.price_oracle,
        "radicle": {
            "exact_binaries": exact_radicle_binaries,
            "public_path": input.radicle,
        },
    });
    let mut bytes = serde_json::to_vec_pretty(&document)?;
    bytes.push(b'\n');
    publish_member(
        evidence_dir,
        &format!("scenario-{:03}.json", input.scenario_id),
        &bytes,
    )
    .wrap_err_with(|| format!("publish scenario evidence in {}", evidence_dir.display()))?;
    Ok(())
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn event_name(event: &ScenarioFinished) -> &'static str {
    match event {
        ScenarioFinished::StepPassed => "passed",
        ScenarioFinished::StepSkipped => "skipped",
        ScenarioFinished::StepFailed(..) => "step_failed",
        ScenarioFinished::BeforeHookFailed(..) => "before_hook_failed",
    }
}

fn git_identity(repo: &Path) -> (Option<String>, Option<bool>, Option<bool>) {
    let sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|sha| sha.trim().to_owned());
    let tracked_dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(repo)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| !output.stdout.is_empty());
    let untracked_dirty = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(repo)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| !output.stdout.is_empty());
    (sha, tracked_dirty, untracked_dirty)
}
