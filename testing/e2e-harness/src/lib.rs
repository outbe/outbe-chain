#![cfg(any(not(feature = "release-sgx-e2e"), feature = "ocomp-integration"))]

//! Rust cucumber harness for the outbe-chain e2e suite.
//!
//! The scenarios live as Gherkin fixtures under `features/`; the step code
//! behind them ([`features`]) drives typed handles ([`world`]). Chain reads and
//! sends are native (alloy [`Provider`]/`sol!`, see `internal::eth`); the
//! committee validators, joiner, followers, and their enclave containers are all
//! launched as Rust-owned processes by one handle ([`world::localnet`], via
//! [`internal::proc`]) - no `run-testnet.sh`/`nohup`. Bootstrap keeps only two
//! one-shot subprocesses (`outbe-chain dkg bootstrap` + `python3 seed_genesis.py`);
//! governance/tribute sends still go through `outbe-cli`.
//!
//! [`Provider`]: https://docs.rs/alloy-provider
//!
//! The [`run`] entry point is driven by the `outbe-e2e` binary: the CLI defines
//! the [`env::Environment`] (validators / TEE mode / sudo), and Gherkin tags
//! define each scenario's requirements.

pub mod artifacts;
pub mod env;
pub mod features;
pub mod localnet_driver;
pub mod metadosis_evidence;
pub mod metadosis_p0;
pub mod metadosis_process;
pub mod mongo_fixture;
pub mod ocomp_capacity;
pub mod ocomp_evidence;
#[cfg(feature = "ocomp-finality-fixture")]
pub mod ocomp_finality_fixture;
pub mod release_dcap;
pub mod release_sgx;
pub mod verification_ledger;
pub mod world;

mod evidence;
mod internal;
mod validator_evidence;

use cucumber::cli;
use cucumber::tag::Ext as _;
use cucumber::writer::Stats;
use cucumber::World as _;
use futures::FutureExt as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::artifacts::ArtifactLedger;
use crate::env::{decide, unmet, Decision, EnvCli, Environment};
use crate::internal::config::Config;
use crate::world::localnet::Localnet;
use crate::world::mongodb::MongoDb;
use crate::world::World;

#[derive(Default)]
struct RunCounters {
    selected: AtomicUsize,
    started: AtomicUsize,
    finished: AtomicUsize,
    evidence: AtomicUsize,
}

#[derive(Clone)]
struct WatchdogHandle {
    shared: Arc<(Mutex<WatchdogState>, Condvar)>,
}

struct WatchdogState {
    active: Option<(String, Instant)>,
    stop: bool,
}

struct ScenarioWatchdog {
    handle: WatchdogHandle,
    thread: Option<thread::JoinHandle<()>>,
}

impl ScenarioWatchdog {
    fn start(env: Environment) -> Self {
        let timeout = Duration::from_secs(env.scenario_timeout_secs);
        let shared = Arc::new((
            Mutex::new(WatchdogState {
                active: None,
                stop: false,
            }),
            Condvar::new(),
        ));
        let handle = WatchdogHandle {
            shared: Arc::clone(&shared),
        };
        let thread = thread::Builder::new()
            .name("outbe-e2e-watchdog".to_owned())
            .spawn(move || watchdog_loop(shared, env, timeout))
            .expect("spawn E2E scenario watchdog");
        Self {
            handle,
            thread: Some(thread),
        }
    }

    fn handle(&self) -> WatchdogHandle {
        self.handle.clone()
    }

    fn stop(mut self) {
        self.handle.stop();
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join E2E scenario watchdog");
        }
    }
}

impl WatchdogHandle {
    fn arm(&self, scenario: String, timeout: Duration) {
        let (state, wake) = &*self.shared;
        let mut state = state.lock().expect("lock E2E watchdog");
        state.active = Some((scenario, Instant::now() + timeout));
        wake.notify_all();
    }

    fn disarm(&self) {
        let (state, wake) = &*self.shared;
        let mut state = state.lock().expect("lock E2E watchdog");
        state.active = None;
        wake.notify_all();
    }

    fn stop(&self) {
        let (state, wake) = &*self.shared;
        let mut state = state.lock().expect("lock E2E watchdog");
        state.stop = true;
        state.active = None;
        wake.notify_all();
    }
}

fn watchdog_loop(
    shared: Arc<(Mutex<WatchdogState>, Condvar)>,
    env: Environment,
    timeout: Duration,
) {
    let (state, wake) = &*shared;
    loop {
        let mut state = state.lock().expect("lock E2E watchdog");
        while state.active.is_none() && !state.stop {
            state = wake.wait(state).expect("wait for E2E scenario");
        }
        if state.stop {
            return;
        }
        let (scenario, deadline) = state.active.clone().expect("active watchdog scenario");
        let now = Instant::now();
        if now < deadline {
            let (next, _) = wake
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .expect("wait for E2E scenario deadline");
            drop(next);
            continue;
        }
        state.active = None;
        drop(state);

        eprintln!(
            "outbe-e2e: scenario {scenario:?} exceeded {}s; retaining diagnostics and tearing down",
            timeout.as_secs()
        );
        if let Err(error) = evidence::write_run_timeout(&env, &scenario, timeout) {
            eprintln!("outbe-e2e: could not write timeout evidence: {error:#}");
        }
        shutdown_and_exit_with_code(&env, 1);
    }
}

/// Tear the localnet down and exit when the process is interrupted
/// (Ctrl-C / SIGINT or SIGTERM).
///
/// Cucumber's per-scenario `after` hook only runs on normal completion, so a
/// signal would otherwise leave the running scenario's committee validators and
/// enclave containers orphaned. On the signal path the `World` is never dropped,
/// so the owned process/enclave guards never fire - we reconstruct the teardown
/// target from the resolved environment (the same data-dir every `World` uses)
/// and run the stateless datadir-scoped sweep before exiting `130` (SIGINT).
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
    MongoDb::teardown_managed_for_run(env);
    std::process::exit(code);
}

/// Parse the CLI, install the environment, and run the cucumber suite over
/// `features/`.
///
/// A scenario whose requirements the environment can't satisfy is **skipped**
/// (a `SKIPPED:` line is printed and it is filtered out). With `--all`, such a
/// scenario instead **fails** - a `before` hook panics so it counts as a hook
/// error. Only one scenario runs at a time (the localnet is a single shared
/// resource). Exits non-zero on any failure.
pub async fn run() {
    // Parse cucumber's built-in flags (--tags/--name/--input) plus our EnvCli.
    let mut opts = cli::Opts::<_, _, _, EnvCli>::parsed();
    let mut environment = Environment::from_cli(&opts.custom);
    // Cucumber 0.21 treats its built-in name/tag filters as alternatives to
    // `filter_run`'s predicate. Move them into our one composed predicate so
    // CLI selection can never bypass environment/@todo policy or accounting.
    let re_filter = opts.re_filter.take();
    let tags_filter = opts.tags_filter.take();

    // Give each run its own data subdir under the base `--data-dir`, and each
    // scenario a `scenario-<n>` subdir under that (see `Config::for_scenario`).
    // The enclave container tag and the teardown sweep both derive from the run
    // dir, so this one move also makes this run's docker names + sweep scope
    // unique - two runs (or a prior crashed one) never touch each other's
    // nodes/containers, with no manual `--data-dir` juggling.
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
    let env_hook = environment.clone();
    let env_filter = environment.clone();
    let env_evidence = environment.clone();
    let artifacts = Arc::new(Mutex::new(ArtifactLedger::new(&environment)));
    let env_cleanup = environment;
    let counters = Arc::new(RunCounters::default());
    let hook_counters = Arc::clone(&counters);
    let after_counters = Arc::clone(&counters);
    let filter_counters = Arc::clone(&counters);
    let hook_artifacts = Arc::clone(&artifacts);
    let watchdog = ScenarioWatchdog::start(env_hook.clone());
    let before_watchdog = watchdog.handle();
    let after_watchdog = watchdog.handle();
    let scenario_timeout = Duration::from_secs(env_hook.scenario_timeout_secs);

    let writer = World::cucumber()
        .max_concurrent_scenarios(1)
        // An undefined step is an invalid acceptance test, not a successful
        // partial scenario. Environment-ineligible scenarios are filtered out
        // before execution and therefore never reach this writer policy.
        .fail_on_skipped()
        .before(move |feature, _rule, scenario, _world| {
            hook_counters.started.fetch_add(1, Ordering::Relaxed);
            before_watchdog.arm(
                format!("{} :: {}", feature.name, scenario.name),
                scenario_timeout,
            );
            // Only reachable for unmet scenarios in `--all` mode (the filter
            // excludes them otherwise); panic so they count as failures.
            let reason = if env_hook.all {
                unmet(feature, scenario, &env_hook)
            } else {
                None
            };
            if reason.is_none() {
                hook_artifacts
                    .lock()
                    .expect("lock E2E artifact ledger")
                    .preflight_scenario(&env_hook, feature, scenario)
                    .unwrap_or_else(|error| panic!("E2E artifact preflight failed: {error:#}"));
            }
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
                let price_oracle = world.price_oracle.evidence_snapshot();
                world.price_oracle.teardown();
                world
                    .localnet
                    .teardown()
                    .unwrap_or_else(|error| panic!("E2E localnet teardown failed: {error:#}"));
                let audit = world.localnet.audit_unexpected_logs(
                    world
                        .state
                        .allow_unsupported_update_fatal
                        .then_some(world.state.proposed_version)
                        .flatten(),
                    world.state.expected_dkg_reveal.as_deref(),
                    world.state.ocomp_full_node_mismatch_job_id,
                    world.state.expected_tee_lease_guard_shutdown_validator,
                    world.state.expected_tee_lease_guard_shutdown_full_node,
                );
                let audit = match audit {
                    Ok(audit) => audit,
                    Err(error) => {
                        panic!("E2E log-safety audit could not run: {error:#}");
                    }
                };
                let ocomp = match world.ocomp.evidence_snapshot() {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        let _ = world.localnet.teardown();
                        panic!("OCOMP topology evidence could not be captured: {error:#}");
                    }
                };
                let mut ocomp_public = world.state.ocomp_public_scenario_evidence();
                if let Some(meter) = world.capacity_meter.take() {
                    if !scenario.tags.iter().any(|tag| tag == "ocomp-capacity") {
                        let _ = world.localnet.teardown();
                        panic!(
                            "dedicated OCOMP capacity meter was used for a non-capacity scenario"
                        );
                    }
                    let cas_roots = ocomp
                        .domain_roots
                        .iter()
                        .map(|root| std::path::PathBuf::from(root).join("cas-v1"))
                        .collect::<Vec<_>>();
                    ocomp_public.capacity_resources =
                        Some(meter.finish(&cas_roots).unwrap_or_else(|error| {
                            let _ = world.localnet.teardown();
                            panic!("OCOMP capacity resource evidence failed: {error:#}");
                        }));
                }
                if let Err(error) = audit.ensure_clean() {
                    panic!("E2E log-safety audit failed: {error:#}");
                }
                if let Err(error) = evidence::write_scenario(evidence::ScenarioEvidence {
                    env: &env_evidence,
                    feature,
                    scenario,
                    event,
                    scenario_id: world.localnet.scenario_id(),
                    scenario_dir: world.localnet.scenario_dir(),
                    elapsed: world.started_at.elapsed(),
                    audit: &audit,
                    gramine_image_id: world.localnet.enclave_image_id(),
                    ocomp: &ocomp,
                    ocomp_public: &ocomp_public,
                    price_oracle: &price_oracle,
                    radicle: &world.state.radicle,
                }) {
                    panic!("E2E evidence write failed: {error:#}");
                }
                after_counters.evidence.fetch_add(1, Ordering::Relaxed);
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
                let cli_selected = re_filter.as_ref().map_or_else(
                    || {
                        tags_filter.as_ref().is_none_or(|tags| {
                            tags.eval(
                                feature
                                    .tags
                                    .iter()
                                    .chain(rule.iter().flat_map(|rule| &rule.tags))
                                    .chain(scenario.tags.iter()),
                            )
                        })
                    },
                    |name| name.is_match(&scenario.name),
                );
                if !cli_selected {
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

    // `execution_has_failed` covers failed steps, parsing errors, and hook errors
    // - so `--all`, which fails unmet scenarios by panicking in the `before` hook,
    // keeps its data dir too.
    let dir = env_cleanup.data_dir.display().to_string();
    if writer.execution_has_failed()
        || !complete
        || evidence_error.is_some()
        || artifact_error.is_some()
        || runtime_cleanup_error.is_some()
        || manifest_result.is_err()
    {
        eprintln!("outbe-e2e: {}", failure_summary(&writer));
        eprintln!("outbe-e2e: incomplete run summary: {summary:?}");
        if let Some(error) = evidence_error {
            eprintln!("outbe-e2e: evidence inventory failed: {error}");
        }
        if let Some(error) = artifact_error {
            eprintln!("outbe-e2e: artifact inventory failed: {error}");
        }
        if let Some(error) = runtime_cleanup_error {
            eprintln!("outbe-e2e: Radicle runtime cleanup failed: {error}");
        }
        if let Err(error) = manifest_result {
            eprintln!("outbe-e2e: final run manifest failed: {error:#}");
        }
        eprintln!("outbe-e2e: data dir kept at {dir}");
        std::process::exit(1);
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

        assert_eq!(
            risk_ids.len(),
            16,
            "one scenario group per live checklist ID"
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
