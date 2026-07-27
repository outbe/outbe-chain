use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cucumber::event::ScenarioFinished;
use cucumber::gherkin::{Feature, Scenario};
use eyre::{Result, WrapErr};
use serde_json::json;

use crate::env::{Environment, TeeMode};
use crate::ocomp_evidence::{hash_file, publish_member};
use crate::world::localnet::LogAudit;
use crate::world::ocomp::OcompScenarioTopologyV1;
use crate::world::state::OcompPublicScenarioEvidenceV1;

pub(crate) struct ScenarioEvidence<'a> {
    pub env: &'a Environment,
    pub feature: &'a Feature,
    pub scenario: &'a Scenario,
    pub event: &'a ScenarioFinished,
    pub scenario_id: usize,
    pub scenario_dir: &'a Path,
    pub elapsed: Duration,
    pub audit: &'a LogAudit,
    pub ocomp: &'a OcompScenarioTopologyV1,
    pub ocomp_public: &'a OcompPublicScenarioEvidenceV1,
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
        let (enclave_name, enclave_path) = match input.env.tee_mode {
            TeeMode::Real => ("outbe_tee_enclave", input.env.real_enclave_bin.as_path()),
            TeeMode::Mock => ("outbe_tee_enclave_mock", input.env.mock_bin.as_path()),
            TeeMode::None => {
                return Err(eyre::eyre!(
                    "OCOMP scenario evidence requires an enabled TEE mode"
                ));
            }
        };
        Some(json!({
            "outbe_chain": hash_file(&input.env.chain_bin)
                .wrap_err("hash exact outbe-chain binary")?,
            "outbe_ocomp": hash_file(&input.env.ocomp_bin)
                .wrap_err("hash exact outbe-ocomp binary")?,
            "outbe_cli": hash_file(&input.env.cli_bin)
                .wrap_err("hash exact outbe-cli binary")?,
            "outbe_keygen": hash_file(&input.env.keygen_bin)
                .wrap_err("hash exact outbe-keygen binary")?,
            (enclave_name): hash_file(enclave_path)
                .wrap_err("hash exact selected outbe-tee-enclave binary")?,
            "outbe_e2e": hash_file(&current_exe)
                .wrap_err("hash exact outbe-e2e binary")?,
        }))
    } else {
        None
    };
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
            "tee": format!("{:?}", input.env.tee_mode).to_ascii_lowercase(),
            "all": input.env.all,
        },
        "scenario_data_dir": input.scenario_dir,
        "log_audit": input.audit.json(),
        "ocomp": {
            "exact_binaries": exact_ocomp_binaries,
            "topology": input.ocomp,
            "public_path": input.ocomp_public,
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
