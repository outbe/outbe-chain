use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cucumber::event::ScenarioFinished;
use cucumber::gherkin::{Feature, Scenario};
use eyre::{Result, WrapErr};
use serde_json::json;

use crate::env::Environment;
use crate::ocomp_evidence::publish_member;
use crate::world::localnet::LogAudit;

pub(crate) struct ScenarioEvidence<'a> {
    pub env: &'a Environment,
    pub feature: &'a Feature,
    pub scenario: &'a Scenario,
    pub event: &'a ScenarioFinished,
    pub scenario_id: usize,
    pub scenario_dir: &'a Path,
    pub elapsed: Duration,
    pub audit: &'a LogAudit,
}

pub(crate) fn write_scenario(input: ScenarioEvidence<'_>) -> Result<()> {
    let evidence_dir = input
        .env
        .evidence_dir
        .as_ref()
        .expect("run() resolves the evidence directory");
    let (sha, tracked_dirty, untracked_dirty) = git_identity(&input.env.repo);
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
