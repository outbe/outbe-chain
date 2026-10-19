//! Per-scenario cucumber hooks: selection, preflight, and teardown evidence.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cucumber::event::ScenarioFinished;
use cucumber::gherkin::tagexpr::TagOperation;
use cucumber::gherkin::{Feature, Rule, Scenario};
use cucumber::tag::Ext as _;

use crate::artifacts::ArtifactLedger;
use crate::env::{self, unmet, Environment};
use crate::evidence;
use crate::watchdog::WatchdogHandle;
use crate::world::localnet::{ExpectedTeeFailures, LogAudit};
use crate::world::World;

pub(crate) struct BeforeScenario {
    pub(crate) env: Environment,
    pub(crate) watchdog: WatchdogHandle,
    pub(crate) artifacts: Arc<Mutex<ArtifactLedger>>,
}

impl BeforeScenario {
    /// Arms the watchdog and preflights artifacts; returns why an `--all`
    /// scenario cannot run in this environment.
    pub(crate) fn begin(
        &self,
        feature: &Feature,
        scenario: &Scenario,
        world: &mut World,
    ) -> Option<String> {
        let deadline = self.watchdog.arm(
            format!("{} :: {}", feature.name, scenario.name),
            Duration::from_secs(env::scenario_timeout_secs(&self.env)),
        );
        world.localnet.set_scenario_deadline(deadline);

        crate::features::entity_lifecycle::markets::configure(world, &scenario.tags);
        // Only reachable for unmet scenarios in `--all` mode (the filter
        // excludes them otherwise). Panic so they count as failures.
        let reason = if self.env.all {
            unmet(feature, scenario, &self.env)
        } else {
            None
        };
        if reason.is_none() {
            self.artifacts
                .lock()
                .expect("lock E2E artifact ledger")
                .preflight_scenario(&self.env, feature, scenario)
                .unwrap_or_else(|error| panic!("E2E artifact preflight failed: {error:#}"));
        }
        reason
    }
}

/// Whether cucumber's own `--name`/`--tags` selection keeps this scenario.
/// A `--name` match result, when given, takes precedence over `--tags`.
pub(crate) fn cli_selected(
    name_match: Option<bool>,
    tags_filter: Option<&TagOperation>,
    feature: &Feature,
    rule: Option<&Rule>,
    scenario: &Scenario,
) -> bool {
    name_match.unwrap_or_else(|| {
        tags_filter.is_none_or(|tags| {
            tags.eval(
                feature
                    .tags
                    .iter()
                    .chain(rule.iter().flat_map(|rule| &rule.tags))
                    .chain(scenario.tags.iter()),
            )
        })
    })
}

/// Tears the scenario down, audits its logs, and publishes its evidence.
/// Returns the audit so the caller can fail the scenario after counting it.
pub(crate) fn finish_scenario(
    env: &Environment,
    feature: &Feature,
    scenario: &Scenario,
    event: &ScenarioFinished,
    world: &mut World,
) -> LogAudit {
    let price_oracle = world.price_oracle.evidence_snapshot();
    if matches!(event, ScenarioFinished::StepPassed) && !price_oracle.feeder_processes.is_empty() {
        crate::world::price_oracle::verify_price_oracle_evidence(
            &price_oracle,
            world.validators.size(),
        )
        .expect("validate native Oracle evidence");
    }
    world.price_oracle.teardown();
    // Drain node-facing clients while their node is still alive,
    // then collect every cleanup result before auditing final logs.
    let node_cleanup = world.localnet.teardown_with_expected_exits(
        &expected_failed_slots(world),
        &world.state.expected_dkg_expiry_exits,
    );
    let mut audit = match audit_scenario_logs(world) {
        Ok(audit) => audit,
        Err(error) => {
            panic!("E2E log-safety audit could not run: {error:#}");
        }
    };
    for error in [node_cleanup.err()].into_iter().flatten() {
        audit.record_cleanup_failure(error);
    }
    if let Err(error) = evidence::write_scenario(evidence::ScenarioEvidence {
        env,
        feature,
        scenario,
        event,
        scenario_id: world.localnet.scenario_id(),
        scenario_dir: world.localnet.scenario_dir(),
        elapsed: world.started_at.elapsed(),
        audit: &audit,
        gramine_image_id: world.localnet.enclave_image_id(),
        price_oracle: &price_oracle,
        radicle: &world.state.radicle,
        tee_lease: &world.state.tee_lease,
        restart_observations: &world.state.restart_observations,
    }) {
        panic!("E2E evidence write failed: {error:#}");
    }
    audit
}

fn expected_failed_slots(world: &World) -> Vec<usize> {
    let mut slots = Vec::new();
    if world.state.allow_unsupported_update_fatal && world.state.proposed_version.is_some() {
        slots.extend(0..world.localnet.committee_size());
    }
    slots.extend(world.state.expected_tee_lease_guard_shutdown_validator);
    if let Some(proof) = &world.state.expected_tee_lease_guard_shutdown_full_node {
        slots.push(proof.slot);
    }
    slots
}

fn audit_scenario_logs(world: &World) -> eyre::Result<LogAudit> {
    world.localnet.audit_unexpected_logs(
        world
            .state
            .allow_unsupported_update_fatal
            .then_some(world.state.proposed_version)
            .flatten(),
        world.state.expected_dkg_reveal.as_deref(),
        ExpectedTeeFailures {
            lease_guard_validator: world.state.expected_tee_lease_guard_shutdown_validator,
            sidecar_outage: world
                .state
                .expected_tee_sidecar_outage
                .as_ref()
                .map(|(index, tx)| (*index, tx.as_str())),
            lease_guard_full_node: world
                .state
                .expected_tee_lease_guard_shutdown_full_node
                .as_ref(),
        },
    )
}
