//! Test **environment** (from the CLI) vs. scenario **requirements** (from tags).
//!
//! The binary's clap flags describe the box we're running on - how many
//! validators to bootstrap, which enclave mode, whether we have `sudo`. Each
//! Gherkin scenario declares what it *needs* via tags. The runner matches the
//! two: a scenario the environment can't satisfy is **skipped**, or - with
//! `--all` - turned into a **failure**. Scenarios pinned to a different explicit
//! TEE execution profile remain skipped even under `--all`; each profile has
//! its own lane.
//!
//! Every requirement is a **tag** (matched on merged feature + scenario tags,
//! `@`-less), so the Given text stays purely descriptive:
//!   - `min-validators-N` -> requires `--validators >= N` (N parsed from the tag).
//!   - `validators-N`     -> requires `--validators == N` (N parsed from the tag).
//!   - `tee`              -> requires an enabled enclave mode.
//!   - `sudo`             -> requires `sudo` (no `--no-sudo`).
//!   - explicit TEE profile tags (`real-sgx`, `sgx-no-attest`,
//!     `gramine-direct`) -> always skipped outside that profile, regardless of
//!     `--all`.
//!   - `todo`             -> always skipped (unimplemented stub), regardless of `--all`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use cucumber::gherkin::{Feature, Scenario};

use crate::internal::ports::Ports;
use crate::metadosis_p0::{
    MetadosisP0Case, MetadosisP0EnvironmentReceiptV1, REMOVED_OWNER_FAILPOINT,
};

/// Enclave mode the localnet runs with (the `--tee` flag).
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TeeMode {
    /// Real SGX under `gramine-sgx` (needs SGX hardware).
    Real,
    /// Production enclave under real `gramine-sgx` with remote attestation
    /// disabled. Uses EGETKEY sealing and a production NodeHost session while
    /// the chain accepts only GramineDirectDev evidence.
    SgxNoAttest,
    /// Production enclave binary under `gramine-direct` (no SGX).
    GramineDirect,
    /// Test-only mock enclave binary under `gramine-direct` (no SGX).
    #[default]
    Mock,
    /// Test-only mock enclave binary as a plain host process - no container, no
    /// Gramine, no LibOS, no attestation. For hosts where Gramine cannot run
    /// (the amd64 image has no arm64 build and dies under emulation), which is
    /// every macOS box. Development only; it proves nothing Gramine proves.
    MockNative,
}

/// Co-located hardware enclaves share one physical EPC, so SGX E2E subprocesses
/// need one wider budget for both individual enclave calls and the complete
/// block-1 TEE bootstrap. Production/testnet defaults are not changed.
pub(crate) const CO_LOCATED_HARDWARE_SGX_TIMEOUT_SECS: u64 = 1_800;

impl TeeMode {
    /// Whether an enclave is launched.
    pub const fn enabled(self) -> bool {
        true
    }

    /// Whether the test-only mock enclave binary is selected.
    pub const fn uses_mock_binary(self) -> bool {
        matches!(self, TeeMode::Mock | TeeMode::MockNative)
    }

    /// Whether the enclave runs as a bare host process instead of inside the
    /// Gramine test container. No image is built, resolved or recorded.
    pub const fn runs_native_host_enclave(self) -> bool {
        matches!(self, TeeMode::MockNative)
    }

    /// Whether SGX device nodes are passed to the Gramine container.
    pub const fn passes_sgx_devices(self) -> bool {
        matches!(self, TeeMode::Real | TeeMode::SgxNoAttest)
    }

    /// Whether the harness supplies a deterministic per-validator DKG seed.
    pub const fn uses_deterministic_dkg_seed(self) -> bool {
        matches!(
            self,
            TeeMode::GramineDirect | TeeMode::Mock | TeeMode::MockNative
        )
    }

    /// Stable CLI/evidence spelling for the selected mode.
    pub const fn evidence_name(self) -> &'static str {
        match self {
            Self::Real => "real",
            Self::SgxNoAttest => "sgx-no-attest",
            Self::GramineDirect => "gramine-direct",
            Self::Mock => "mock",
            Self::MockNative => "mock-native",
        }
    }

    /// Whether this mode satisfies a scenario's explicit `@gramine-direct`
    /// execution requirement.
    pub const fn satisfies_gramine_direct_requirement(self) -> bool {
        matches!(self, Self::GramineDirect)
    }

    /// Whether this mode proves the explicit production SGX-without-DCAP
    /// profile rather than merely running a development transport.
    pub const fn satisfies_sgx_no_attest_requirement(self) -> bool {
        matches!(self, Self::SgxNoAttest)
    }
}

/// The clap arguments that define the environment (merged with cucumber's own
/// `--tags`/`--name`/`--input` via [`cucumber::cli::Opts`]).
///
/// Everything is a CLI flag. The test-only `--metadosis-p0-case` additionally
/// validates the removed variable inherited by node children, but never uses it
/// as product configuration. Path flags are optional and default relative to
/// `--repo`.
#[derive(clap::Args, Clone, Debug)]
pub struct EnvCli {
    /// Number of committee validators to bootstrap.
    #[arg(long, default_value_t = 4)]
    pub validators: usize,

    /// Don't probe for free ports - take each node's block verbatim.
    ///
    /// By default every node's block of 7 ports (rpc, tee, p2p, discv5, authrpc,
    /// metrics, consensus) is scanned for: the allocator walks forward past any
    /// busy port, so a parallel or coexisting run finds a free set. (Each parallel
    /// run still needs its own `--data-dir`.) With this flag the blocks are the
    /// static `18545 + i * 7` layout and a busy port surfaces as a launch failure.
    /// Either way each scenario's blocks sit above the previous scenario's.
    #[arg(long)]
    pub no_resolve_ports: bool,

    /// Enclave mode for the localnet.
    #[arg(long, value_enum, default_value_t = TeeMode::Mock)]
    pub tee: TeeMode,

    /// Run docker/process/script steps without `sudo`.
    #[arg(long, conflicts_with = "force_sudo")]
    pub no_sudo: bool,

    /// Force docker/process/script steps through `sudo`, even when Docker is
    /// reachable directly. SGX device access may require this independently of
    /// Docker socket permissions.
    #[arg(long = "sudo", conflicts_with = "no_sudo")]
    pub force_sudo: bool,

    /// Treat a scenario the environment can't satisfy as a FAILURE instead of
    /// skipping it.
    #[arg(long)]
    pub all: bool,

    /// Stream localnet setup output (bootstrap / node launch / docker) live.
    /// Off by default: that output is captured and only surfaced on failure.
    #[arg(long)]
    pub debug: bool,

    /// Keep the run's data dir even when every scenario passed. A run with any
    /// failure always keeps it, so its chain state and logs stay inspectable.
    #[arg(long)]
    pub no_cleanup: bool,

    /// Repo root (working dir for scripts/binaries). Defaults to this crate's
    /// workspace root.
    #[arg(long)]
    pub repo: Option<PathBuf>,

    /// Base localnet data dir (defaults to `/tmp/outbe-e2e-harness`). Each run
    /// lands in a unique `run-<secs>-<pid>` subdir under it, so concurrent runs
    /// self-isolate (own data + docker names + teardown scope).
    #[arg(long)]
    pub data_dir: Option<PathBuf>,

    /// Persistent JSON evidence directory. Defaults to `<data-dir>/evidence/<run-id>`;
    /// unlike scenario data, it is retained after successful cleanup.
    #[arg(long)]
    pub evidence_dir: Option<PathBuf>,

    /// Build manifest produced by `outbe-e2e-build` for the exact binaries
    /// admitted into this run. Required for every executable scenario.
    #[arg(long)]
    pub artifact_manifest: Option<PathBuf>,

    /// Hard wall-clock deadline for one scenario, including setup, teardown,
    /// evidence capture, and log audit. A timeout tears down owned processes
    /// and exits non-zero with a durable timeout record.
    #[arg(long, default_value_t = 3_600)]
    pub scenario_timeout_secs: u64,

    /// Exact Metadosis P0 parity case. Test-only: validates and retains the
    /// removed process input inherited by every node child.
    #[arg(long, value_enum)]
    pub metadosis_p0_case: Option<MetadosisP0Case>,

    /// `outbe-chain` binary. Defaults to `<repo>/target/release/outbe-chain`.
    #[arg(long)]
    pub chain_bin: Option<PathBuf>,

    /// `outbe-ocomp` binary. Defaults to `<repo>/target/release/outbe-ocomp`.
    #[arg(long)]
    pub ocomp_bin: Option<PathBuf>,

    /// `outbe-feeder` binary. Defaults to `<repo>/target/release/outbe-feeder`.
    #[arg(long)]
    pub feeder_bin: Option<PathBuf>,

    /// Optional prebuilt newer `outbe-chain` binary for operator replacement.
    /// When omitted, the update E2E builds the requested version itself from a
    /// temporary worktree of the source revision under test.
    #[arg(long)]
    pub upgraded_chain_bin: Option<PathBuf>,

    /// `outbe-cli` binary. Defaults to `<repo>/target/release/outbe-cli`.
    #[arg(long)]
    pub cli_bin: Option<PathBuf>,

    /// `outbe-keygen` binary. Defaults to `<repo>/target/release/outbe-keygen`.
    #[arg(long)]
    pub keygen_bin: Option<PathBuf>,

    /// Production enclave binary used by `real` and `gramine-direct`. Defaults
    /// to `<repo>/target/release/outbe-tee-enclave`.
    #[arg(long)]
    pub enclave_bin: Option<PathBuf>,

    /// Mock enclave binary. Defaults to
    /// `<repo>/target/release/outbe-tee-enclave-mock`.
    #[arg(long)]
    pub mock_bin: Option<PathBuf>,

    /// Genesis seed file. Defaults to
    /// `<repo>/scripts/seed-testnet-lowstake.json`.
    #[arg(long)]
    pub seed: Option<PathBuf>,

    /// Transaction-capable MongoDB URI shared by the harness. When omitted, the
    /// harness owns a temporary `mongo:7.0` single-node replica-set container.
    #[arg(long, default_value = "auto")]
    pub projection_mongodb_uri: String,
}

/// The resolved environment: every knob and path the harness needs, sourced
/// entirely from the CLI.
#[derive(Clone, Debug)]
pub struct Environment {
    pub validators: usize,
    /// Per-node port blocks, shared by every scenario's `World`. Each scenario
    /// calls [`Ports::start_scenario`], which re-seeds the committee above the
    /// previous scenario's blocks; the joiner and followers take the next block
    /// on first use.
    pub(crate) ports: Ports,
    /// Keep the run's data dir even on a fully successful run.
    pub no_cleanup: bool,
    pub tee_mode: TeeMode,
    pub sudo: bool,
    pub all: bool,
    /// Stream localnet setup output live (else capture, show only on failure).
    pub debug: bool,
    pub repo: PathBuf,
    pub data_dir: PathBuf,
    pub evidence_dir: Option<PathBuf>,
    pub artifact_manifest: Option<PathBuf>,
    pub scenario_timeout_secs: u64,
    pub metadosis_p0: Option<MetadosisP0EnvironmentReceiptV1>,
    pub chain_bin: PathBuf,
    pub ocomp_bin: PathBuf,
    pub feeder_bin: PathBuf,
    pub upgraded_chain_bin: Option<PathBuf>,
    pub cli_bin: PathBuf,
    pub keygen_bin: PathBuf,
    pub enclave_bin: PathBuf,
    pub mock_bin: PathBuf,
    pub seed: PathBuf,
    pub projection_mongodb_uri: String,
}

impl Environment {
    /// Resolve from the parsed CLI. Unset path flags default relative to the
    /// repo root. The sole process-environment read is the explicit P0 evidence
    /// receipt selected by `--metadosis-p0-case`.
    pub fn from_cli(cli: &EnvCli) -> Self {
        let repo = cli.repo.clone().unwrap_or_else(default_repo);
        let metadosis_p0 = cli.metadosis_p0_case.map(|case| {
            MetadosisP0EnvironmentReceiptV1::capture(case).unwrap_or_else(|error| {
                panic!(
                    "invalid --metadosis-p0-case environment receipt for \
                     {REMOVED_OWNER_FAILPOINT}: {error:#}"
                )
            })
        });
        Self {
            validators: cli.validators,
            ports: Ports::new(!cli.no_resolve_ports),
            no_cleanup: cli.no_cleanup,
            tee_mode: cli.tee,
            sudo: resolve_sudo(cli.force_sudo, cli.no_sudo),
            all: cli.all,
            debug: cli.debug,
            data_dir: cli.data_dir.clone().unwrap_or_else(default_data_dir),
            evidence_dir: cli.evidence_dir.clone(),
            artifact_manifest: cli.artifact_manifest.clone(),
            scenario_timeout_secs: cli.scenario_timeout_secs,
            metadosis_p0,
            chain_bin: cli
                .chain_bin
                .clone()
                .unwrap_or_else(|| repo.join("target/release/outbe-chain")),
            ocomp_bin: cli
                .ocomp_bin
                .clone()
                .unwrap_or_else(|| repo.join("target/release/outbe-ocomp")),
            feeder_bin: cli
                .feeder_bin
                .clone()
                .unwrap_or_else(|| repo.join("target/release/outbe-feeder")),
            upgraded_chain_bin: cli.upgraded_chain_bin.clone(),
            cli_bin: cli
                .cli_bin
                .clone()
                .unwrap_or_else(|| repo.join("target/release/outbe-cli")),
            keygen_bin: cli
                .keygen_bin
                .clone()
                .unwrap_or_else(|| repo.join("target/release/outbe-keygen")),
            enclave_bin: cli
                .enclave_bin
                .clone()
                .unwrap_or_else(|| repo.join("target/release/outbe-tee-enclave")),
            mock_bin: cli
                .mock_bin
                .clone()
                .unwrap_or_else(|| repo.join("target/release/outbe-tee-enclave-mock")),
            seed: cli
                .seed
                .clone()
                .unwrap_or_else(|| repo.join("scripts/seed-testnet-lowstake.json")),
            projection_mongodb_uri: cli.projection_mongodb_uri.clone(),
            repo,
        }
    }

    /// Exact enclave binary selected by the configured execution mode.
    pub fn selected_enclave_bin(&self) -> &Path {
        if self.tee_mode.uses_mock_binary() {
            &self.mock_bin
        } else {
            &self.enclave_bin
        }
    }
}

impl Default for Environment {
    fn default() -> Self {
        Self::from_cli(&EnvCli {
            validators: 4,
            // Unlike the CLI defaults, don't scan and never delete: a `Default`
            // environment must be deterministic, must not bind sockets, and must
            // not remove anything (it is used by unit tests).
            no_resolve_ports: true,
            no_cleanup: true,
            tee: TeeMode::Mock,
            no_sudo: false,
            force_sudo: false,
            all: false,
            debug: false,
            repo: None,
            data_dir: None,
            evidence_dir: None,
            artifact_manifest: None,
            scenario_timeout_secs: 3_600,
            metadosis_p0_case: None,
            chain_bin: None,
            ocomp_bin: None,
            feeder_bin: None,
            upgraded_chain_bin: None,
            cli_bin: None,
            keygen_bin: None,
            enclave_bin: None,
            mock_bin: None,
            seed: None,
            projection_mongodb_uri: "auto".to_owned(),
        })
    }
}

/// Whether this user can already drive the Docker daemon unprivileged, so the
/// harness must not prepend `sudo` and prompt for a password it does not need.
///
/// True for Docker Desktop (macOS) and for any Linux host whose user is in the
/// `docker` group; false for a rootful daemon, which still gets `sudo`. Mirrors
/// `scripts/localnet-mongo.sh`. Probed once - the daemon does not change
/// reachability mid-run, and every `base_cmd` would otherwise pay for it.
fn docker_reachable_without_sudo() -> bool {
    static REACHABLE: OnceLock<bool> = OnceLock::new();
    *REACHABLE.get_or_init(|| {
        std::process::Command::new("docker")
            .arg("info")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

fn resolve_sudo(force_sudo: bool, no_sudo: bool) -> bool {
    force_sudo || (!no_sudo && !docker_reachable_without_sudo())
}

/// Default repo root: two levels up from this crate (`testing/e2e-harness`).
fn default_repo() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..2 {
        p.pop();
    }
    p
}

/// Keep Unix-domain socket paths below the platform `sun_path` limit. In
/// particular, macOS expands `std::env::temp_dir()` to a long per-user path;
/// appending run/scenario/validator components makes reth.ipc exceed 104 bytes.
fn default_data_dir() -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from("/tmp/outbe-e2e-harness")
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir().join("outbe-e2e-harness")
    }
}

static ENV: OnceLock<Environment> = OnceLock::new();

static SCENARIO_SEQ: AtomicUsize = AtomicUsize::new(0);

/// The 1-based id of the next scenario to build a `World`, naming its data
/// subdir (`scenario-<id>`). Skipped scenarios never build a `World`, so ids
/// count the scenarios that actually ran.
pub(crate) fn next_scenario_id() -> usize {
    SCENARIO_SEQ.fetch_add(1, Ordering::Relaxed) + 1
}

/// Install the resolved environment (called once by `run()` before cucumber
/// constructs any `World`).
pub fn set_environment(env: Environment) {
    let _ = ENV.set(env);
}

/// The active environment, or a sensible default (used by lib unit tests that
/// never call [`set_environment`]).
pub fn environment() -> Environment {
    ENV.get().cloned().unwrap_or_default()
}

/// Whether the scenario is an unimplemented stub (`@todo`).
pub fn is_todo(feature: &Feature, scenario: &Scenario) -> bool {
    has_tag(feature, scenario, "todo")
}

/// Why the environment can't satisfy this scenario, or `None` if it can.
///
/// Every requirement is declared as a tag (`@tee`, validator count, `@sudo`),
/// so the Given text stays purely descriptive - nothing here reparses step prose.
pub fn unmet(feature: &Feature, scenario: &Scenario, env: &Environment) -> Option<String> {
    if let Some(n) = exact_validators(feature, scenario) {
        if env.validators != n {
            return Some(format!(
                "needs exactly {n} validators, have {}",
                env.validators
            ));
        }
    }
    if let Some(n) = required_validators(feature, scenario) {
        if env.validators < n {
            return Some(format!("needs >={n} validators, have {}", env.validators));
        }
    }
    if has_tag(feature, scenario, "gramine-direct")
        && !env.tee_mode.satisfies_gramine_direct_requirement()
    {
        return Some(format!(
            "needs the production enclave under gramine-direct (@gramine-direct), but --tee {}",
            env.tee_mode.evidence_name()
        ));
    }
    if has_tag(feature, scenario, "real-sgx") && !matches!(env.tee_mode, TeeMode::Real) {
        return Some(format!(
            "needs SGX hardware and DcapRequired (@real-sgx), but --tee {}",
            env.tee_mode.evidence_name()
        ));
    }
    if has_tag(feature, scenario, "sgx-no-attest")
        && !env.tee_mode.satisfies_sgx_no_attest_requirement()
    {
        return Some(format!(
            "needs production SGX without remote attestation (@sgx-no-attest), but --tee {}",
            env.tee_mode.evidence_name()
        ));
    }
    if has_tag(feature, scenario, "sudo") && !env.sudo {
        return Some("needs sudo (@sudo), but --no-sudo".to_string());
    }
    if has_tag(feature, scenario, "ocomp") && !cfg!(feature = "ocomp-integration") {
        return Some(
            "needs the Rust OCOMP integration profile (@ocomp), but the harness was built without --features ocomp-integration"
                .to_string(),
        );
    }
    None
}

/// The exact validator count from a `@validators-N` tag, if present.
pub fn exact_validators(feature: &Feature, scenario: &Scenario) -> Option<usize> {
    feature
        .tags
        .iter()
        .chain(scenario.tags.iter())
        .find_map(|tag| parse_exact_validators_tag(tag))
}

/// The minimum validator count from a `@min-validators-N` tag, if present.
pub fn required_validators(feature: &Feature, scenario: &Scenario) -> Option<usize> {
    feature
        .tags
        .iter()
        .chain(scenario.tags.iter())
        .find_map(|tag| parse_min_validators_tag(tag))
}

/// Whether the scenario requires an enclave (`@tee`).
pub fn requires_tee(feature: &Feature, scenario: &Scenario) -> bool {
    has_tag(feature, scenario, "tee")
}

/// Parse `N` out of a `min-validators-<N>` tag (tags are `@`-less here).
fn parse_min_validators_tag(tag: &str) -> Option<usize> {
    tag.strip_prefix("min-validators-")?.parse().ok()
}

/// Parse `N` out of a `validators-<N>` tag (tags are `@`-less here).
fn parse_exact_validators_tag(tag: &str) -> Option<usize> {
    tag.strip_prefix("validators-")?.parse().ok()
}

/// What to do with a scenario given the environment.
#[derive(Debug, Eq, PartialEq)]
pub enum Decision {
    Run,
    Skip(String),
}

/// Decide run vs skip. `@todo` always skips; an unmet requirement skips unless
/// `--all` (then it runs so the `before` hook can fail it).
pub fn decide(feature: &Feature, scenario: &Scenario, env: &Environment) -> Decision {
    if is_todo(feature, scenario) {
        return Decision::Skip("not implemented (@todo)".to_string());
    }
    let requirement = unmet(feature, scenario, env);
    let profile_mismatch = (has_tag(feature, scenario, "real-sgx")
        && !matches!(env.tee_mode, TeeMode::Real))
        || (has_tag(feature, scenario, "sgx-no-attest")
            && !env.tee_mode.satisfies_sgx_no_attest_requirement())
        || (has_tag(feature, scenario, "gramine-direct")
            && !env.tee_mode.satisfies_gramine_direct_requirement());
    decide_requirement(requirement, env.all, profile_mismatch)
}

fn decide_requirement(requirement: Option<String>, run_all: bool, force_skip: bool) -> Decision {
    match requirement {
        None => Decision::Run,
        Some(reason) if force_skip => Decision::Skip(reason),
        Some(reason) if run_all => {
            // Run it; the `before` hook panics so it counts as a failure.
            let _ = reason;
            Decision::Run
        }
        Some(reason) => Decision::Skip(reason),
    }
}

fn has_tag(feature: &Feature, scenario: &Scenario, tag: &str) -> bool {
    feature
        .tags
        .iter()
        .chain(scenario.tags.iter())
        .any(|t| t == tag)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn settlement_scenarios_declare_their_integration_build_requirement() {
        let feature = Feature::parse_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("features/settlement.feature"),
            cucumber::gherkin::GherkinEnv::default(),
        )
        .expect("parse settlement feature");
        assert_eq!(feature.scenarios.len(), 2);
        assert!(
            feature.tags.iter().any(|tag| tag == "ocomp"),
            "settlement step registration requires the OCOMP integration build"
        );
        let mut env = Environment {
            tee_mode: TeeMode::SgxNoAttest,
            validators: 4,
            sudo: true,
            ..Environment::default()
        };
        for scenario in &feature.scenarios {
            if cfg!(feature = "ocomp-integration") {
                assert_eq!(unmet(&feature, scenario, &env), None);
                assert_eq!(decide(&feature, scenario, &env), Decision::Run);
            } else {
                assert!(unmet(&feature, scenario, &env)
                    .expect("missing build requirement")
                    .contains("built without --features ocomp-integration"));
                assert!(matches!(
                    decide(&feature, scenario, &env),
                    Decision::Skip(_)
                ));
            }
            env.all = true;
            assert_eq!(decide(&feature, scenario, &env), Decision::Run);
            assert_eq!(
                unmet(&feature, scenario, &env).is_some(),
                !cfg!(feature = "ocomp-integration"),
                "--all must expose missing capabilities to the failing before hook"
            );
            env.all = false;
        }
    }

    fn assert_registered_steps(feature: &Feature, scenario: &Scenario) {
        use cucumber::World as _;

        let steps = crate::world::World::collection();
        for step in feature
            .background
            .iter()
            .flat_map(|background| &background.steps)
            .chain(&scenario.steps)
        {
            assert!(
                steps
                    .find(step)
                    .unwrap_or_else(|error| panic!("ambiguous step in {}: {error}", scenario.name))
                    .is_some(),
                "unregistered {:?} step in {}: {}",
                step.ty,
                scenario.name,
                step.value
            );
        }
    }

    #[test]
    fn chained_followers_keep_all_twelve_live_handoff_and_restart_steps() {
        let env = Environment {
            tee_mode: TeeMode::SgxNoAttest,
            validators: 4,
            sudo: true,
            all: true,
            ..Environment::default()
        };
        let feature = Feature::parse_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("features/fullnode.feature"),
            cucumber::gherkin::GherkinEnv::default(),
        )
        .unwrap();
        let scenario = feature.scenarios.iter().find(|scenario| scenario.name == "Chained FullNodes stop on upstream loss and recover through a healthy upstream").unwrap();
        assert_eq!(scenario.steps.len(), 12);
        assert_eq!(unmet(&feature, scenario, &env), None);
        assert_eq!(decide(&feature, scenario, &env), Decision::Run);
        assert_registered_steps(&feature, scenario);
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn worker_outage_fault_is_after_public_input_and_includes_independent_recovery() {
        let feature = Feature::parse_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("features/ocomp.feature"),
            cucumber::gherkin::GherkinEnv::default(),
        )
        .unwrap();
        let scenario = feature
            .scenarios
            .iter()
            .find(|scenario| {
                scenario.name
                    == "Complete worker outage preserves exports and cannot halt consensus"
            })
            .unwrap();
        assert_eq!(scenario.steps.len(), 10);
        assert_eq!(scenario.steps[4].value, "all four OCOMP workers stop after exact exports of the public JobIntent before voting opens");
        assert_eq!(
            scenario.steps[9].value,
            "the independent OCOMP job completes on every validator"
        );
        assert_registered_steps(&feature, scenario);
    }

    #[test]
    fn active_pending_validator_restart_is_selected_with_all_eight_registered_steps() {
        let env = Environment {
            tee_mode: TeeMode::SgxNoAttest,
            validators: 4,
            sudo: true,
            all: true,
            ..Environment::default()
        };
        let feature = Feature::parse_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("features/txpool_eviction.feature"),
            cucumber::gherkin::GherkinEnv::default(),
        )
        .unwrap();
        let selected: Vec<_> = feature
            .scenarios
            .iter()
            .filter(|scenario| has_tag(&feature, scenario, "pending-validator-restart"))
            .collect();
        assert_eq!(selected.len(), 1);
        let scenario = selected[0];
        assert_eq!(scenario.steps.len(), 8);
        assert_eq!(unmet(&feature, scenario, &env), None);
        assert_eq!(decide(&feature, scenario, &env), Decision::Run);
        assert_registered_steps(&feature, scenario);
    }

    #[test]
    fn ordinary_oracle_and_zerofee_rollover_steps_remain_available_without_integration() {
        let env = Environment {
            tee_mode: TeeMode::SgxNoAttest,
            validators: 4,
            sudo: true,
            ..Environment::default()
        };
        let mut checked = Vec::new();
        for file in ["price_oracle.feature", "zerofee.feature"] {
            let feature = Feature::parse_path(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("features")
                    .join(file),
                cucumber::gherkin::GherkinEnv::default(),
            )
            .expect("parse ordinary feeder-dependent fixture");
            for scenario in &feature.scenarios {
                if has_tag(&feature, scenario, "price-oracle")
                    || has_tag(&feature, scenario, "pfs-007-12")
                {
                    assert_eq!(unmet(&feature, scenario, &env), None);
                    assert_eq!(decide(&feature, scenario, &env), Decision::Run);
                    assert!(!has_tag(&feature, scenario, "ocomp"));
                    assert_registered_steps(&feature, scenario);
                    checked.push(scenario.name.clone());
                }
            }
        }
        assert_eq!(
            checked,
            [
                "Per-pair quorum survives a sub-quorum cross intersection",
                "Exhausted quota resets lazily across the worldwide-day boundary",
            ]
        );
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn settlement_and_nod_redemption_keep_all_three_executable_scenarios() {
        let env = Environment {
            tee_mode: TeeMode::SgxNoAttest,
            validators: 4,
            sudo: true,
            all: true,
            ..Environment::default()
        };
        let mut checked = Vec::new();
        for file in ["settlement.feature", "ocomp.feature"] {
            let feature = Feature::parse_path(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("features")
                    .join(file),
                cucumber::gherkin::GherkinEnv::default(),
            )
            .expect("parse settlement integration fixture");
            for scenario in &feature.scenarios {
                if has_tag(&feature, scenario, "settlement")
                    || has_tag(&feature, scenario, "nod-settlement")
                {
                    assert_eq!(unmet(&feature, scenario, &env), None);
                    assert_eq!(decide(&feature, scenario, &env), Decision::Run);
                    assert_registered_steps(&feature, scenario);
                    checked.push(scenario.name.clone());
                }
            }
        }
        assert_eq!(checked, [
            "A zero-balance validator redeems its reward Gem through ZeroFee",
            "A stale Oracle rate defers validator Gem delivery and later recovers",
            "A public Tribute completes real OCOMP, FullNode verification, NOD, replay, and contributor payout",
        ]);
    }

    #[test]
    fn gramine_direct_uses_the_production_enclave_without_sgx_passthrough() {
        let mode = TeeMode::GramineDirect;
        assert!(mode.enabled());
        assert!(!mode.uses_mock_binary());
        assert!(!mode.passes_sgx_devices());
        assert!(mode.uses_deterministic_dkg_seed());
        assert_eq!(mode.evidence_name(), "gramine-direct");
        assert!(mode.satisfies_gramine_direct_requirement());
        assert!(!TeeMode::Mock.satisfies_gramine_direct_requirement());
        assert!(!TeeMode::Real.satisfies_gramine_direct_requirement());
    }

    /// The native profile runs the same mock binary and the same deterministic
    /// seed, but outside Gramine - so it must carry its own evidence label and
    /// must not stand in for any profile that proves a Gramine or SGX property.
    #[test]
    fn native_host_mode_is_a_distinct_profile_that_proves_no_gramine_property() {
        let mode = TeeMode::MockNative;
        assert!(mode.enabled());
        assert!(mode.uses_mock_binary());
        assert!(mode.runs_native_host_enclave());
        assert!(mode.uses_deterministic_dkg_seed());
        assert!(!mode.passes_sgx_devices());
        assert_eq!(mode.evidence_name(), "mock-native");
        assert!(!mode.satisfies_gramine_direct_requirement());
        assert!(!mode.satisfies_sgx_no_attest_requirement());

        // Every containerized profile keeps running under Gramine.
        for containerized in [
            TeeMode::Real,
            TeeMode::SgxNoAttest,
            TeeMode::GramineDirect,
            TeeMode::Mock,
        ] {
            assert!(
                !containerized.runs_native_host_enclave(),
                "{containerized:?} must stay containerized"
            );
            assert_ne!(containerized.evidence_name(), mode.evidence_name());
        }
    }

    /// Both mock profiles select the mock binary; only the wrapper differs.
    #[test]
    fn native_host_mode_selects_the_mock_enclave_binary() {
        let env = Environment {
            tee_mode: TeeMode::MockNative,
            mock_bin: PathBuf::from("/artifact-set/outbe-tee-enclave-mock"),
            enclave_bin: PathBuf::from("/artifact-set/outbe-tee-enclave"),
            ..Environment::default()
        };
        assert_eq!(
            env.selected_enclave_bin(),
            Path::new("/artifact-set/outbe-tee-enclave-mock")
        );
    }

    #[test]
    fn gramine_direct_selects_the_exact_production_enclave_binary() {
        let env = Environment {
            tee_mode: TeeMode::GramineDirect,
            enclave_bin: PathBuf::from("/artifact-set/outbe-tee-enclave"),
            mock_bin: PathBuf::from("/artifact-set/outbe-tee-enclave-mock"),
            ..Environment::default()
        };
        assert_eq!(
            env.selected_enclave_bin(),
            Path::new("/artifact-set/outbe-tee-enclave")
        );
    }

    #[cfg(unix)]
    #[test]
    fn default_data_dir_keeps_unix_socket_paths_short() {
        let path = default_data_dir()
            .join("run-1785161738-87200")
            .join("scenario-1/validator-0/data/reth.ipc");

        assert!(path.as_os_str().len() < 104, "{}", path.display());
    }

    #[test]
    fn sgx_no_attest_uses_production_enclave_and_real_sgx_without_dev_seed() {
        let mode = TeeMode::SgxNoAttest;
        assert!(mode.enabled());
        assert!(!mode.uses_mock_binary());
        assert!(mode.passes_sgx_devices());
        assert!(!mode.uses_deterministic_dkg_seed());
        assert_eq!(mode.evidence_name(), "sgx-no-attest");
        assert!(mode.satisfies_sgx_no_attest_requirement());
        assert!(!TeeMode::Real.satisfies_sgx_no_attest_requirement());
        assert!(!TeeMode::GramineDirect.satisfies_sgx_no_attest_requirement());

        let env = Environment {
            tee_mode: mode,
            enclave_bin: PathBuf::from("/artifact-set/outbe-tee-enclave"),
            mock_bin: PathBuf::from("/artifact-set/outbe-tee-enclave-mock"),
            ..Environment::default()
        };
        assert_eq!(
            env.selected_enclave_bin(),
            Path::new("/artifact-set/outbe-tee-enclave")
        );
    }

    #[test]
    fn explicit_sudo_overrides_docker_reachability() {
        assert!(resolve_sudo(true, false));
        assert!(!resolve_sudo(false, true));
    }

    #[test]
    fn parses_min_validators_tag() {
        assert_eq!(parse_min_validators_tag("min-validators-4"), Some(4));
        assert_eq!(parse_min_validators_tag("min-validators-12"), Some(12));
        assert_eq!(parse_min_validators_tag("tee"), None);
        assert_eq!(parse_min_validators_tag("min-validators-"), None);
        assert_eq!(parse_min_validators_tag("min-validators-x"), None);
    }

    #[test]
    fn parses_exact_validators_tag() {
        assert_eq!(parse_exact_validators_tag("validators-4"), Some(4));
        assert_eq!(parse_exact_validators_tag("validators-12"), Some(12));
        assert_eq!(parse_exact_validators_tag("min-validators-4"), None);
        assert_eq!(parse_exact_validators_tag("validators-"), None);
        assert_eq!(parse_exact_validators_tag("validators-x"), None);
    }

    #[test]
    fn exact_validator_requirement_rejects_a_larger_committee() {
        let feature = Feature::parse_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("features/price_oracle.feature"),
            cucumber::gherkin::GherkinEnv::default(),
        )
        .expect("parse price Oracle feature");
        let scenario = feature.scenarios.first().expect("price Oracle scenario");
        let mut env = Environment {
            validators: 4,
            ..Environment::default()
        };
        assert_eq!(unmet(&feature, scenario, &env), None);

        env.validators = 5;
        assert_eq!(
            unmet(&feature, scenario, &env).as_deref(),
            Some("needs exactly 4 validators, have 5")
        );
    }

    #[test]
    fn real_sgx_requirement_stays_skipped_under_all() {
        let reason = "needs DcapRequired".to_string();
        assert_eq!(
            decide_requirement(Some(reason.clone()), true, true),
            Decision::Skip(reason)
        );
        assert_eq!(
            decide_requirement(Some("ordinary requirement".to_string()), true, false),
            Decision::Run
        );
    }

    #[test]
    fn parsed_real_sgx_scenario_is_skipped_by_sgx_no_attest_all_lane() {
        let feature = Feature::parse_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("features/tee_onboarding.feature"),
            cucumber::gherkin::GherkinEnv::default(),
        )
        .expect("parse TEE onboarding feature");
        let scenario = feature.scenarios.first().expect("onboarding scenario");
        let env = Environment {
            tee_mode: TeeMode::SgxNoAttest,
            all: true,
            ..Environment::default()
        };

        assert!(matches!(
            decide(&feature, scenario, &env),
            Decision::Skip(_)
        ));
    }

    #[test]
    fn explicit_tee_profiles_are_disjoint_even_under_all() {
        let no_attest_feature = Feature::parse_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("features/tribute.feature"),
            cucumber::gherkin::GherkinEnv::default(),
        )
        .expect("parse SGX-no-attest feature");
        let no_attest = no_attest_feature
            .scenarios
            .first()
            .expect("SGX-no-attest scenario");
        let direct_feature = Feature::parse_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("features/intex.feature"),
            cucumber::gherkin::GherkinEnv::default(),
        )
        .expect("parse gramine-direct feature");
        let direct = direct_feature
            .scenarios
            .first()
            .expect("gramine-direct scenario");

        let no_attest_env = Environment {
            tee_mode: TeeMode::SgxNoAttest,
            all: true,
            sudo: true,
            ..Environment::default()
        };
        assert_eq!(
            decide(&no_attest_feature, no_attest, &no_attest_env),
            Decision::Run
        );
        assert!(matches!(
            decide(&direct_feature, direct, &no_attest_env),
            Decision::Skip(_)
        ));

        let direct_env = Environment {
            tee_mode: TeeMode::GramineDirect,
            all: true,
            ..Environment::default()
        };
        assert_eq!(decide(&direct_feature, direct, &direct_env), Decision::Run);
        assert!(matches!(
            decide(&no_attest_feature, no_attest, &direct_env),
            Decision::Skip(_)
        ));
    }
}
