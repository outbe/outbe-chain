//! Test **environment** (from the CLI) vs. scenario **requirements** (from tags).
//!
//! The binary's clap flags describe the box we're running on — how many
//! validators to bootstrap, which enclave mode, whether we have `sudo`. Each
//! Gherkin scenario declares what it *needs* via tags. The runner matches the
//! two: a scenario the environment can't satisfy is **skipped**, or — with
//! `--all` — turned into a **failure**.
//!
//! Every requirement is a **tag** (matched on merged feature + scenario tags,
//! `@`-less), so the Given text stays purely descriptive:
//!   - `min-validators-N` → requires `--validators >= N` (N parsed from the tag).
//!   - `tee`              → requires an enabled enclave mode.
//!   - `sudo`             → requires `sudo` (no `--no-sudo`).
//!   - `todo`             → always skipped (unimplemented stub), regardless of `--all`.

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
    /// Production enclave binary under `gramine-direct` (no SGX).
    GramineDirect,
    /// Test-only mock enclave binary under `gramine-direct` (no SGX).
    #[default]
    Mock,
}

impl TeeMode {
    /// Whether an enclave is launched.
    pub const fn enabled(self) -> bool {
        true
    }

    /// Whether the test-only mock enclave binary is selected.
    pub const fn uses_mock_binary(self) -> bool {
        matches!(self, TeeMode::Mock)
    }

    /// Whether SGX device nodes are passed to the Gramine container.
    pub const fn passes_sgx_devices(self) -> bool {
        matches!(self, TeeMode::Real)
    }

    /// Whether the harness supplies a deterministic per-validator DKG seed.
    pub const fn uses_deterministic_dkg_seed(self) -> bool {
        matches!(self, TeeMode::GramineDirect | TeeMode::Mock)
    }

    /// Stable CLI/evidence spelling for the selected mode.
    pub const fn evidence_name(self) -> &'static str {
        match self {
            Self::Real => "real",
            Self::GramineDirect => "gramine-direct",
            Self::Mock => "mock",
        }
    }

    /// Whether this mode satisfies a scenario's explicit `@gramine-direct`
    /// execution requirement.
    pub const fn satisfies_gramine_direct_requirement(self) -> bool {
        matches!(self, Self::GramineDirect)
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

    /// Don't probe for free ports — take each node's block verbatim.
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
    #[arg(long)]
    pub no_sudo: bool,

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

    /// Exact Metadosis P0 parity case. Test-only: validates and retains the
    /// removed process input inherited by every node child.
    #[arg(long, value_enum)]
    pub metadosis_p0_case: Option<MetadosisP0Case>,

    /// `outbe-chain` binary. Defaults to `<repo>/target/debug/outbe-chain`.
    #[arg(long)]
    pub chain_bin: Option<PathBuf>,

    /// `outbe-ocomp` binary. Defaults to `<repo>/target/debug/outbe-ocomp`.
    #[arg(long)]
    pub ocomp_bin: Option<PathBuf>,

    /// Optional prebuilt newer `outbe-chain` binary for operator replacement.
    /// When omitted, the update E2E builds the requested version itself from a
    /// temporary worktree of the source revision under test.
    #[arg(long)]
    pub upgraded_chain_bin: Option<PathBuf>,

    /// `outbe-cli` binary. Defaults to `<repo>/target/debug/outbe-cli`.
    #[arg(long)]
    pub cli_bin: Option<PathBuf>,

    /// `outbe-keygen` binary. Defaults to `<repo>/target/debug/outbe-keygen`.
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
    pub metadosis_p0: Option<MetadosisP0EnvironmentReceiptV1>,
    pub chain_bin: PathBuf,
    pub ocomp_bin: PathBuf,
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
            sudo: !cli.no_sudo,
            all: cli.all,
            debug: cli.debug,
            data_dir: cli.data_dir.clone().unwrap_or_else(|| {
                std::env::temp_dir().join(format!("outbe-e2e-harness-{}", std::process::id()))
            }),
            evidence_dir: cli.evidence_dir.clone(),
            metadosis_p0,
            chain_bin: cli
                .chain_bin
                .clone()
                .unwrap_or_else(|| repo.join("target/debug/outbe-chain")),
            ocomp_bin: cli
                .ocomp_bin
                .clone()
                .unwrap_or_else(|| repo.join("target/debug/outbe-ocomp")),
            upgraded_chain_bin: cli.upgraded_chain_bin.clone(),
            cli_bin: cli
                .cli_bin
                .clone()
                .unwrap_or_else(|| repo.join("target/debug/outbe-cli")),
            keygen_bin: cli
                .keygen_bin
                .clone()
                .unwrap_or_else(|| repo.join("target/debug/outbe-keygen")),
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
            all: false,
            debug: false,
            repo: None,
            data_dir: None,
            evidence_dir: None,
            metadosis_p0_case: None,
            chain_bin: None,
            ocomp_bin: None,
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

/// Default repo root: two levels up from this crate (`testing/e2e-harness`).
fn default_repo() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..2 {
        p.pop();
    }
    p
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
/// Every requirement is declared as a tag (`@tee`, `@min-validators-N`, `@sudo`),
/// so the Given text stays purely descriptive — nothing here reparses step prose.
pub fn unmet(feature: &Feature, scenario: &Scenario, env: &Environment) -> Option<String> {
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

/// What to do with a scenario given the environment.
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
    match unmet(feature, scenario, env) {
        None => Decision::Run,
        Some(reason) if env.all => {
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

    #[test]
    fn parses_min_validators_tag() {
        assert_eq!(parse_min_validators_tag("min-validators-4"), Some(4));
        assert_eq!(parse_min_validators_tag("min-validators-12"), Some(12));
        assert_eq!(parse_min_validators_tag("tee"), None);
        assert_eq!(parse_min_validators_tag("min-validators-"), None);
        assert_eq!(parse_min_validators_tag("min-validators-x"), None);
    }
}
