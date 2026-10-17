#![cfg(any(not(feature = "release-sgx-e2e"), feature = "ocomp-integration"))]

//! Rust cucumber harness for the outbe-chain e2e suite.
//!
//! The scenarios live as Gherkin fixtures under `features/`. The step code
//! behind them ([`features`]) drives typed handles ([`world`]). Chain reads and
//! sends are native (alloy [`Provider`]/`sol!`, see `internal::eth`). One handle
//! ([`world::localnet`], via [`internal::proc`]) launches the committee
//! validators, joiner, followers, and their enclave containers as Rust-owned
//! processes, with no `run-testnet.sh`/`nohup`. Bootstrap keeps only two
//! one-shot subprocesses (`outbe-chain dkg bootstrap` + `python3 seed_genesis.py`).
//! Governance/tribute sends still go through `outbe-cli`.
//!
//! [`Provider`]: https://docs.rs/alloy-provider
//!
//! The `outbe-e2e` binary drives the [`run`] entry point. The CLI defines the
//! [`env::Environment`] (validators / TEE mode / sudo), and Gherkin tags
//! define each scenario's requirements.

pub mod artifacts;
pub mod env;
pub mod features;
pub mod localnet_driver;
pub mod metadosis_evidence;
pub mod metadosis_p0;
pub mod metadosis_process;
pub mod ocomp_capacity;
pub mod ocomp_evidence;
pub mod release_dcap;
pub mod release_sgx;
pub mod verification_ledger;
pub mod world;

mod evidence;
mod internal;
mod scenario_hooks;
mod validator_evidence;
mod watchdog;

use cucumber::cli;
use cucumber::writer::Stats;
use cucumber::World as _;
use futures::FutureExt as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::artifacts::ArtifactLedger;
use crate::env::{decide, Decision, EnvCli, Environment};
use crate::internal::config::Config;
use crate::scenario_hooks::{cli_selected, BeforeScenario};
use crate::watchdog::ScenarioWatchdog;
use crate::world::localnet::Localnet;
use crate::world::World;

#[derive(Default)]
struct RunCounters {
    selected: AtomicUsize,
    started: AtomicUsize,
    finished: AtomicUsize,
    evidence: AtomicUsize,
}

/// Tear the localnet down and exit when the process is interrupted
/// (Ctrl-C / SIGINT or SIGTERM).
///
/// Cucumber's per-scenario `after` hook only runs on normal completion, so a
/// signal would otherwise leave the running scenario's committee validators and
/// enclave containers orphaned. On the signal path, nothing drops the `World`,
/// so the owned process/enclave guards never fire. This handler reconstructs the
/// teardown target from the resolved environment (the same data-dir every
/// `World` uses). It then runs the stateless datadir-scoped sweep before it
/// exits with `130` (SIGINT).
async fn teardown_on_signal(env: Environment) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            // If we can't install the SIGTERM handler, still honour Ctrl-C.
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                shutdown_and_exit(&env);
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    shutdown_and_exit(&env);
}

/// Run the shared localnet teardown for `env` and exit the process. Never
/// returns.
fn shutdown_and_exit(env: &Environment) -> ! {
    eprintln!("\noutbe-e2e: interrupted - tearing down the localnet...");
    shutdown_and_exit_with_code(env, 130)
}

fn shutdown_and_exit_with_code(env: &Environment, code: i32) -> ! {
    // Best-effort: the shutdown is itself best-effort (ignores already-stopped
    // nodes / missing containers), so a partially-started run is safe to tear
    // down too.
    if let Err(error) = Localnet::new(Config::resolve(env)).teardown() {
        eprintln!("outbe-e2e: Radicle runtime cleanup failed during shutdown: {error:#}");
    }
    std::process::exit(code);
}

/// Parse the CLI, install the environment, and run the cucumber suite over
/// `features/`.
///
/// A scenario whose requirements the environment can't satisfy is **skipped**
/// (the runner prints a `SKIPPED:` line and excludes it). With `--all`, such
/// a scenario instead **fails**. A `before` hook panics so it counts as a hook
/// error. Only one scenario runs at a time (the localnet is a single shared
/// resource). Exits non-zero on any failure.
pub async fn run() {
    // Parse cucumber's built-in flags (--tags/--name/--input) plus our EnvCli.
    let mut opts = cli::Opts::<_, _, _, EnvCli>::parsed();
    // Initialize the canonical CRS before any scenario. CRS setup may block
    // on a file read/download. Keep it off the Tokio runtime.
    std::thread::spawn(outbe_zk_backend::barretenberg::BarretenbergHandler::init)
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        .expect("initialize the E2E proving CRS before any scenario");
    let mut environment = Environment::from_cli(&opts.custom);
    // Cucumber 0.21 treats its built-in name/tag filters as alternatives to
    // `filter_run`'s predicate. Move them into our one composed predicate so
    // CLI selection can never bypass environment/@todo policy or accounting.
    let re_filter = opts.re_filter.take();
    let tags_filter = opts.tags_filter.take();

    scope_to_run_dir(&mut environment);

    if let Err(error) = evidence::write_run_started(&environment) {
        eprintln!("outbe-e2e: failed to publish run-started evidence: {error:#}");
        std::process::exit(1);
    }

    env::set_environment(environment.clone());

    // Tear the localnet down on Ctrl-C / SIGTERM so an interrupted run never
    // leaves committee validators or enclave containers orphaned (the cucumber
    // `after` hook only fires on normal completion).
    tokio::spawn(teardown_on_signal(environment.clone()));

    // Hand an owned clone to each `'static` closure.
    let env_filter = environment.clone();
    let env_evidence = environment.clone();
    let artifacts = Arc::new(Mutex::new(ArtifactLedger::new(&environment)));
    let env_cleanup = environment.clone();
    let counters = Arc::new(RunCounters::default());
    let hook_counters = Arc::clone(&counters);
    let after_counters = Arc::clone(&counters);
    let filter_counters = Arc::clone(&counters);
    let watchdog = ScenarioWatchdog::start(environment.clone());
    let before = BeforeScenario {
        env: environment,
        watchdog: watchdog.handle(),
        artifacts: Arc::clone(&artifacts),
    };
    let after_watchdog = watchdog.handle();

    let writer = World::cucumber()
        .max_concurrent_scenarios(1)
        // An undefined step is an invalid acceptance test, not a successful
        // partial scenario. Environment-ineligible scenarios are filtered out
        // before execution and therefore never reach this writer policy.
        .fail_on_skipped()
        .before(move |feature, _rule, scenario, world| {
            hook_counters.started.fetch_add(1, Ordering::Relaxed);
            let reason = before.begin(feature, scenario, world);
            async move {
                if let Some(reason) = reason {
                    panic!("environment cannot satisfy this scenario: {reason}");
                }
            }
            .boxed_local()
        })
        // Tear the localnet down after every scenario (pass or fail) so the
        // network/enclave containers never outlive the run. Stop it before the
        // log audit: a failed boundary can otherwise keep emitting the same
        // fatal while the audit walks growing files. Skipped scenarios build no
        // `World`, so there is nothing to stop.
        .after(move |feature, _rule, scenario, event, world| {
            if let Some(world) = world {
                let audit =
                    scenario_hooks::finish_scenario(&env_evidence, feature, scenario, event, world);
                after_counters.evidence.fetch_add(1, Ordering::Relaxed);
                if let Err(error) = audit.ensure_clean() {
                    panic!("E2E log-safety audit failed: {error:#}");
                }
            }
            after_counters.finished.fetch_add(1, Ordering::Relaxed);
            after_watchdog.disarm();
            async move {}.boxed_local()
        })
        .with_cli(opts)
        // Absolute path so the runner finds fixtures regardless of CWD (cargo
        // run executes from the workspace root).
        //
        // `filter_run` rather than `filter_run_and_exit`: the latter panics on
        // failure and never returns, leaving nowhere to hang the cleanup below.
        .filter_run(
            concat!(env!("CARGO_MANIFEST_DIR"), "/features"),
            move |feature, rule, scenario| {
                let name_match = re_filter.as_ref().map(|name| name.is_match(&scenario.name));
                if !cli_selected(name_match, tags_filter.as_ref(), feature, rule, scenario) {
                    return false;
                }

                match decide(feature, scenario, &env_filter) {
                    Decision::Run => {
                        filter_counters.selected.fetch_add(1, Ordering::Relaxed);
                        true
                    }
                    Decision::Skip(reason) => {
                        println!("SKIPPED: {} - {reason}", scenario.name);
                        false
                    }
                }
            },
        )
        .await;

    watchdog.stop();

    let runtime_cleanup_error = Localnet::new(Config::resolve(&env_cleanup))
        .cleanup_radicle_runtime()
        .err()
        .map(|error| format!("{error:#}"));

    let scenario_stats = *writer.scenarios_stats();
    let evidence_records = evidence::scenario_record_count(&env_cleanup);
    let evidence_error = evidence_records
        .as_ref()
        .err()
        .map(|error| format!("{error:#}"));
    let summary = evidence::RunSummary {
        selected: counters.selected.load(Ordering::Relaxed),
        started: counters.started.load(Ordering::Relaxed),
        finished: counters.finished.load(Ordering::Relaxed),
        framework_finished: scenario_stats.total(),
        passed: scenario_stats.passed,
        skipped: scenario_stats.skipped,
        failed: scenario_stats.failed,
        evidence_records: evidence_records.unwrap_or_default(),
        parsing_errors: writer.parsing_errors(),
        hook_errors: writer.hook_errors(),
    };
    let artifact_snapshot = artifacts
        .lock()
        .expect("lock final E2E artifact ledger")
        .snapshot(&env_cleanup);
    let artifact_error = artifact_snapshot
        .as_ref()
        .err()
        .map(|error| format!("{error:#}"));
    let manifest_result = artifact_snapshot
        .and_then(|artifacts| evidence::write_run_manifest(&env_cleanup, summary, &artifacts));
    let complete = summary.is_complete_pass();
    let run_errors = [
        evidence_error.map(|error| format!("evidence inventory failed: {error}")),
        artifact_error.map(|error| format!("artifact inventory failed: {error}")),
        runtime_cleanup_error.map(|error| format!("Radicle runtime cleanup failed: {error}")),
        manifest_result
            .err()
            .map(|error| format!("final run manifest failed: {error:#}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();

    // `execution_has_failed` covers failed steps, parsing errors, and hook
    // errors. So `--all`, which fails unmet scenarios by panicking in the
    // `before` hook, keeps its data dir too.
    let dir = env_cleanup.data_dir.display().to_string();
    if writer.execution_has_failed() || !complete || !run_errors.is_empty() {
        fail_run(&failure_summary(&writer), summary, run_errors, &dir);
    }
    if !env_cleanup.no_cleanup {
        // Every node and enclave is already down (the `after` hook tore each
        // scenario down), so nothing holds the logs open.
        match Localnet::new(Config::resolve(&env_cleanup)).wipe() {
            Ok(()) => eprintln!("outbe-e2e: removed data dir {dir}"),
            Err(e) => eprintln!("outbe-e2e: could not remove data dir {dir}: {e}"),
        }
    }
}

/// Give each run its own data subdir under the base `--data-dir`, and each
/// scenario a `scenario-<n>` subdir under that (see `Config::for_scenario`).
/// The enclave container tag and the teardown sweep both derive from the run
/// dir, so this one move also makes this run's docker names + sweep scope
/// unique. Two runs (or a prior crashed one) never touch each other's
/// nodes/containers, with no manual `--data-dir` juggling.
fn scope_to_run_dir(environment: &mut Environment) {
    let run_id = {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("run-{secs}-{}", std::process::id())
    };
    let base_data_dir = environment.data_dir.clone();
    environment.data_dir = base_data_dir.join(&run_id);
    environment.evidence_dir = Some(
        environment
            .evidence_dir
            .clone()
            .unwrap_or_else(|| base_data_dir.join("evidence").join(&run_id)),
    );
    eprintln!("outbe-e2e: data dir {}", environment.data_dir.display());
    eprintln!(
        "outbe-e2e: evidence dir {}",
        environment.evidence_dir.as_ref().unwrap().display()
    );
}

fn fail_run(
    failures: &str,
    summary: evidence::RunSummary,
    run_errors: Vec<String>,
    dir: &str,
) -> ! {
    eprintln!("outbe-e2e: {failures}");
    eprintln!("outbe-e2e: incomplete run summary: {summary:?}");
    for error in run_errors {
        eprintln!("outbe-e2e: {error}");
    }
    eprintln!("outbe-e2e: data dir kept at {dir}");
    std::process::exit(1);
}

/// The failure tally `filter_run_and_exit` would have panicked with.
fn failure_summary(writer: &impl Stats<World>) -> String {
    let counts = [
        ("step", writer.failed_steps()),
        ("parsing error", writer.parsing_errors()),
        ("hook error", writer.hook_errors()),
    ];
    counts
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(what, n)| {
            let s = if *n > 1 { "s" } else { "" };
            match *what {
                "step" => format!("{n} step{s} failed"),
                _ => format!("{n} {what}{s}"),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod validator_lifecycle_suite_contract {
    use std::collections::BTreeSet;

    use cucumber::gherkin::{Feature, GherkinEnv, Scenario};

    const LIFECYCLE_FEATURE: &str =
        include_str!("../features/validator_lifecycle_consistency.feature");

    fn expanded_examples(scenario: &Scenario) -> usize {
        if scenario.examples.is_empty() {
            return 1;
        }
        scenario
            .examples
            .iter()
            .map(|examples| {
                examples
                    .table
                    .as_ref()
                    .map(|table| table.rows.len().saturating_sub(1))
                    .unwrap_or_default()
            })
            .sum()
    }

    #[test]
    fn lifecycle_feature_has_exact_public_path_coverage_contract() {
        let feature =
            Feature::parse(LIFECYCLE_FEATURE, GherkinEnv::default()).expect("parse lifecycle FSM");
        let risk_ids = feature
            .scenarios
            .iter()
            .flat_map(|scenario| scenario.tags.iter())
            .filter(|tag| tag.starts_with("risk-"))
            .cloned()
            .collect::<BTreeSet<_>>();

        // D-04 and S-03's three examples were unknown-selector checks, not
        // malformed-boundary coverage. S-02 retains that public-ABI contract.
        let expected = [
            "risk-d-01a",
            "risk-d-01b",
            "risk-d-02",
            "risk-d-05",
            "risk-d-06",
            "risk-d-07",
            "risk-d-10",
            "risk-s-01",
            "risk-s-02",
            "risk-s-06",
            "risk-s-08",
            "risk-s-09",
            "risk-s-12",
            "risk-s-14",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
        assert_eq!(risk_ids, expected, "exact public-path scenario inventory");
        assert_eq!(
            feature
                .scenarios
                .iter()
                .map(expanded_examples)
                .sum::<usize>(),
            14
        );
        assert!(!LIFECYCLE_FEATURE.contains("@todo"));
    }

    #[test]
    fn expected_failure_filter_is_empty() {
        let feature =
            Feature::parse(LIFECYCLE_FEATURE, GherkinEnv::default()).expect("parse lifecycle FSM");
        let selected = feature
            .scenarios
            .iter()
            .filter(|scenario| scenario.tags.iter().any(|tag| tag == "expected-to-fail"))
            .map(expanded_examples)
            .sum::<usize>();

        assert_eq!(selected, 0);
    }
}
