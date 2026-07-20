use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cucumber::event::ScenarioFinished;
use cucumber::gherkin::{Feature, Scenario};
use eyre::{Result, WrapErr};
use serde_json::json;

use crate::env::Environment;
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
    fs::create_dir_all(evidence_dir)
        .wrap_err_with(|| format!("create evidence dir {}", evidence_dir.display()))?;
    let (sha, dirty) = git_identity(&input.env.repo);
    let document = json!({
        "schema_version": 1,
        "recorded_at_unix_ms": unix_millis(),
        "source": { "sha": sha, "dirty": dirty },
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
    let target = evidence_dir.join(format!("scenario-{:03}.json", input.scenario_id));
    let temporary = evidence_dir.join(format!(".scenario-{:03}.json.tmp", input.scenario_id));
    fs::write(&temporary, serde_json::to_vec_pretty(&document)?)
        .wrap_err_with(|| format!("write evidence {}", temporary.display()))?;
    fs::rename(&temporary, &target)
        .wrap_err_with(|| format!("publish evidence {}", target.display()))?;
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

fn git_identity(repo: &Path) -> (Option<String>, Option<bool>) {
    let sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|sha| sha.trim().to_owned());
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(repo)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| !output.stdout.is_empty());
    (sha, dirty)
}
