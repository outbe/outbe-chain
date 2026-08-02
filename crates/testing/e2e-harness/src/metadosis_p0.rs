//! Fail-closed verifier for the Metadosis P0 process-environment parity lane.
//!
//! The runner executes the existing maximum-shaped public OCOMP scenario six
//! times: debug/release node artifacts under unset, removed named, and arbitrary
//! process environments. This module binds every run to its actual inherited
//! environment, immutable Final fixture, exact binaries, and consensus outputs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use alloy_primitives::B256;
use eyre::{ensure, Context as _, Result};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::ocomp_evidence::{sha256_hex, MemberDigestV1};
use crate::world::rpc::BlockCommitmentV1;

pub const METADOSIS_P0_EVIDENCE_KIND: &str = "outbe.metadosis-p0-parity";
pub const METADOSIS_P0_SCHEMA_VERSION: u16 = 1;
pub const REMOVED_OWNER_FAILPOINT: &str = "OUTBE_E2E_OCOMP_OWNER_FAILPOINT";
pub const METADOSIS_P0_SCENARIO: &str =
    "The canonical Final devnet processes a shard-cap-plus-one population";

const METADOSIS_P0_FEATURE: &str = "Off-chain computation public path";
const NAMED_FAILPOINT_VALUE: &str = "nod_receipt_root";
const ARBITRARY_FAILPOINT_VALUE: &str = "metadosis-p0-arbitrary-unrecognized-value";
const FINAL_FIXTURE: &str = "crates/testing/e2e-harness/fixtures/ocomp-final-v1";
const REQUIRED_BINARY_NAMES: [&str; 6] = [
    "outbe_chain",
    "outbe_ocomp",
    "outbe_cli",
    "outbe_keygen",
    "outbe_e2e",
    "outbe_tee_enclave",
];

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum MetadosisP0Case {
    DebugUnset,
    DebugNamed,
    DebugArbitrary,
    ReleaseUnset,
    ReleaseNamed,
    ReleaseArbitrary,
}

impl MetadosisP0Case {
    pub const ALL: [Self; 6] = [
        Self::DebugUnset,
        Self::DebugNamed,
        Self::DebugArbitrary,
        Self::ReleaseUnset,
        Self::ReleaseNamed,
        Self::ReleaseArbitrary,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::DebugUnset => "debug_unset",
            Self::DebugNamed => "debug_named",
            Self::DebugArbitrary => "debug_arbitrary",
            Self::ReleaseUnset => "release_unset",
            Self::ReleaseNamed => "release_named",
            Self::ReleaseArbitrary => "release_arbitrary",
        }
    }

    #[must_use]
    pub const fn cli_value(self) -> &'static str {
        match self {
            Self::DebugUnset => "debug-unset",
            Self::DebugNamed => "debug-named",
            Self::DebugArbitrary => "debug-arbitrary",
            Self::ReleaseUnset => "release-unset",
            Self::ReleaseNamed => "release-named",
            Self::ReleaseArbitrary => "release-arbitrary",
        }
    }

    const fn expected_value(self) -> Option<&'static str> {
        match self {
            Self::DebugUnset | Self::ReleaseUnset => None,
            Self::DebugNamed | Self::ReleaseNamed => Some(NAMED_FAILPOINT_VALUE),
            Self::DebugArbitrary | Self::ReleaseArbitrary => Some(ARBITRARY_FAILPOINT_VALUE),
        }
    }

    const fn uses_debug_node(self) -> bool {
        matches!(
            self,
            Self::DebugUnset | Self::DebugNamed | Self::DebugArbitrary
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetadosisP0EnvironmentReceiptV1 {
    pub case: MetadosisP0Case,
    pub variable: String,
    pub present: bool,
    pub value_sha256: Option<String>,
}

impl MetadosisP0EnvironmentReceiptV1 {
    #[must_use]
    pub fn expected(case: MetadosisP0Case) -> Self {
        let value = case.expected_value();
        Self {
            case,
            variable: REMOVED_OWNER_FAILPOINT.to_owned(),
            present: value.is_some(),
            value_sha256: value.map(|value| sha256_hex(value.as_bytes())),
        }
    }

    /// Capture the actual parent environment that every node child inherits.
    pub fn capture(case: MetadosisP0Case) -> Result<Self> {
        let actual = std::env::var_os(REMOVED_OWNER_FAILPOINT)
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| eyre::eyre!("removed P0 env input is not UTF-8"))
            })
            .transpose()?;
        ensure!(
            actual.as_deref() == case.expected_value(),
            "{} expects {}={:?}, observed {:?}",
            case.label(),
            REMOVED_OWNER_FAILPOINT,
            case.expected_value(),
            actual
        );
        Ok(Self::expected(case))
    }
}

#[derive(Clone, Debug)]
pub struct MetadosisP0CaseInput {
    pub case: MetadosisP0Case,
    pub evidence_dir: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetadosisP0ConsensusObservationV1 {
    pub job_id: B256,
    pub result_digest: B256,
    pub q_forming_transaction_hash: B256,
    pub q_forming_block_number: u64,
    pub q_forming_block_hash: B256,
    pub q_forming_receipt_success: bool,
    pub q_forming_receipt_sha256: String,
    pub q_forming_validator_receipt_sha256: Vec<String>,
    pub q_forming_state_root: B256,
    pub q_forming_ce_root: B256,
    pub q_forming_validator_commitments: Vec<BlockCommitmentV1>,
    pub canonical_import_validator_count: u8,
    pub canonical_import_verified: bool,
    pub tribute_count: u64,
    pub nod_count: u64,
    pub worker_shard_count: u64,
    pub transaction_bytes: u64,
    pub block_bytes: u64,
    pub gas: u64,
    pub internal_work: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetadosisP0LaunchIdentityV1 {
    pub chain_id: u64,
    pub genesis_hash: String,
    pub protocol_bundle_hash: String,
    pub fork_install_hash: String,
    pub classification: String,
    pub activation_height: u64,
    pub metadosis_storage_layout_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetadosisP0CaseEvidenceV1 {
    pub case: MetadosisP0Case,
    pub scenario_path: String,
    pub scenario_sha256: String,
    pub scenario_data_dir: String,
    pub node_binary_sha256: String,
    pub genesis_sha256: String,
    pub environment: MetadosisP0EnvironmentReceiptV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetadosisP0ParityEvidenceV1 {
    pub kind: &'static str,
    pub schema_version: u16,
    pub source_revision: String,
    pub feature_tree_sha256: String,
    pub canonical_final_fixture_sha256: String,
    pub canonical_genesis_sha256: String,
    pub gramine_image_id: String,
    pub ocomp_launch_identity: MetadosisP0LaunchIdentityV1,
    pub exact_tool_binaries_sha256: BTreeMap<String, String>,
    pub debug_node_sha256: String,
    pub release_node_sha256: String,
    pub cases: Vec<MetadosisP0CaseEvidenceV1>,
    pub consensus: MetadosisP0ConsensusObservationV1,
}

#[derive(Debug, Deserialize)]
struct ScenarioSourceV1 {
    sha: String,
    tracked_dirty: bool,
    untracked_dirty: bool,
}

#[derive(Debug, Deserialize)]
struct ScenarioEnvironmentV1 {
    validators: u8,
    tee: String,
    all: bool,
    gramine_image_id: String,
}

#[derive(Debug, Deserialize)]
struct ScenarioP0ReceiptV1 {
    environment: MetadosisP0EnvironmentReceiptV1,
    genesis: MemberDigestV1,
    canonical_final_fixture_sha256: String,
}

#[derive(Debug, Deserialize)]
struct ScenarioTopologyV1 {
    launch_identity: MetadosisP0LaunchIdentityV1,
}

#[derive(Debug, Deserialize)]
struct ScenarioPublicPathV1 {
    capacity_public_path: MetadosisP0ConsensusObservationV1,
}

#[derive(Debug, Deserialize)]
struct ScenarioOcompV1 {
    exact_binaries: BTreeMap<String, MemberDigestV1>,
    topology: ScenarioTopologyV1,
    public_path: ScenarioPublicPathV1,
}

#[derive(Debug, Deserialize)]
struct ScenarioRecordV1 {
    source: ScenarioSourceV1,
    invocation: Vec<String>,
    feature: String,
    scenario: String,
    scenario_id: u64,
    result: String,
    environment: ScenarioEnvironmentV1,
    scenario_data_dir: String,
    metadosis_p0: ScenarioP0ReceiptV1,
    ocomp: ScenarioOcompV1,
}

/// Run the exact normal-node feature query used by the P0 verifier.
pub fn capture_outbe_chain_feature_tree(repo: &Path) -> Result<Vec<u8>> {
    let output = Command::new("cargo")
        .args([
            "tree",
            "--locked",
            "-p",
            "outbe-chain",
            "-e",
            "features",
            "-i",
            "outbe-metadosis",
        ])
        .current_dir(repo)
        .output()
        .wrap_err("execute exact outbe-chain feature query")?;
    ensure!(
        output.status.success(),
        "exact outbe-chain feature query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    validate_feature_tree(&output.stdout)?;
    Ok(output.stdout)
}

/// Bind a closure run to a clean exact checkout.
pub fn current_clean_git_revision(repo: &Path) -> Result<String> {
    let status = Command::new("git")
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .current_dir(repo)
        .output()
        .wrap_err("inspect P0 verifier checkout")?;
    ensure!(status.status.success(), "git status failed");
    ensure!(
        status.stdout.is_empty(),
        "P0 closure requires a clean exact source checkout"
    );
    let revision = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .wrap_err("resolve P0 verifier source revision")?;
    ensure!(revision.status.success(), "git rev-parse HEAD failed");
    let revision = std::str::from_utf8(&revision.stdout)
        .wrap_err("git revision is not UTF-8")?
        .trim()
        .to_owned();
    ensure!(is_git_revision(&revision), "git revision is malformed");
    Ok(revision)
}

/// Hash every checked-in canonical Final input in path-and-content order.
pub fn canonical_final_fixture_sha256(repo: &Path) -> Result<String> {
    let root = repo.join(FINAL_FIXTURE);
    let mut files = WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()
        .wrap_err_with(|| format!("walk canonical Final fixture {}", root.display()))?;
    files.retain(|entry| entry.file_type().is_file());
    files.sort_by_key(|entry| entry.path().to_path_buf());
    ensure!(!files.is_empty(), "canonical Final fixture is empty");
    let mut preimage = Vec::new();
    for entry in files {
        let relative = entry
            .path()
            .strip_prefix(&root)
            .wrap_err("canonical Final member escaped fixture root")?;
        let relative = relative
            .to_str()
            .ok_or_else(|| eyre::eyre!("canonical Final member path is not UTF-8"))?;
        let bytes = std::fs::read(entry.path())
            .wrap_err_with(|| format!("read canonical Final member {}", entry.path().display()))?;
        preimage.extend_from_slice(&(relative.len() as u64).to_be_bytes());
        preimage.extend_from_slice(relative.as_bytes());
        preimage.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        preimage.extend_from_slice(&bytes);
    }
    Ok(sha256_hex(&preimage))
}

pub fn verify_metadosis_p0_parity(
    expected_source_revision: &str,
    feature_tree_bytes: &[u8],
    debug_node: &Path,
    release_node: &Path,
    expected_final_fixture_sha256: &str,
    cases: &[MetadosisP0CaseInput],
) -> Result<MetadosisP0ParityEvidenceV1> {
    ensure!(
        is_git_revision(expected_source_revision),
        "expected source revision is malformed"
    );
    ensure!(
        cases.len() == MetadosisP0Case::ALL.len(),
        "P0 parity requires exactly six scenario cases"
    );
    let observed_cases = cases.iter().map(|case| case.case).collect::<Vec<_>>();
    ensure!(
        observed_cases == MetadosisP0Case::ALL,
        "P0 parity cases must be ordered as {:?}, got {observed_cases:?}",
        MetadosisP0Case::ALL
    );
    validate_feature_tree(feature_tree_bytes)?;
    ensure!(
        is_sha256(expected_final_fixture_sha256),
        "canonical Final fixture digest is malformed"
    );

    let debug_bytes = std::fs::read(debug_node)
        .wrap_err_with(|| format!("read debug node {}", debug_node.display()))?;
    let release_bytes = std::fs::read(release_node)
        .wrap_err_with(|| format!("read release node {}", release_node.display()))?;
    let removed_failpoint = REMOVED_OWNER_FAILPOINT.as_bytes();
    ensure!(
        !contains_subslice(&debug_bytes, removed_failpoint),
        "debug node artifact still contains the removed process failpoint"
    );
    ensure!(
        !contains_subslice(&release_bytes, removed_failpoint),
        "release node artifact still contains the removed process failpoint"
    );
    let debug_node_sha256 = sha256_hex(&debug_bytes);
    let release_node_sha256 = sha256_hex(&release_bytes);

    let mut canonical = None;
    let mut canonical_genesis = None;
    let mut canonical_gramine_image = None;
    let mut canonical_launch_identity = None;
    let mut canonical_tool_binaries = None;
    let mut case_evidence = Vec::with_capacity(cases.len());
    let mut evidence_directories = BTreeSet::new();
    let mut scenario_paths = BTreeSet::new();
    let mut scenario_digests = BTreeSet::new();
    let mut scenario_data_dirs = BTreeSet::new();

    for case in cases {
        let evidence_directory = std::fs::canonicalize(&case.evidence_dir)
            .wrap_err_with(|| format!("canonicalize {}", case.evidence_dir.display()))?;
        ensure!(
            evidence_directories.insert(evidence_directory),
            "{} reuses an evidence directory",
            case.case.label()
        );
        let (scenario_path, scenario_bytes, scenario) =
            load_single_p0_scenario(&case.evidence_dir)?;
        let scenario_path = std::fs::canonicalize(&scenario_path)
            .wrap_err_with(|| format!("canonicalize {}", scenario_path.display()))?;
        ensure!(
            scenario_paths.insert(scenario_path.clone()),
            "{} reuses a scenario member",
            case.case.label()
        );
        let scenario_sha256 = sha256_hex(&scenario_bytes);
        ensure!(
            scenario_digests.insert(scenario_sha256.clone()),
            "{} reuses byte-identical scenario evidence",
            case.case.label()
        );
        validate_scenario_envelope(
            case.case,
            expected_source_revision,
            expected_final_fixture_sha256,
            &scenario,
        )?;
        ensure!(
            scenario_data_dirs.insert(scenario.scenario_data_dir.clone()),
            "{} reuses a scenario data directory",
            case.case.label()
        );

        let expected_binary = if case.case.uses_debug_node() {
            &debug_node_sha256
        } else {
            &release_node_sha256
        };
        let binary = scenario
            .ocomp
            .exact_binaries
            .get("outbe_chain")
            .expect("validated exact binary set");
        ensure!(
            &binary.sha256 == expected_binary,
            "{} scenario used node {}, expected {}",
            case.case.label(),
            binary.sha256,
            expected_binary
        );
        let tool_binaries = scenario
            .ocomp
            .exact_binaries
            .iter()
            .filter(|(name, _)| name.as_str() != "outbe_chain")
            .map(|(name, digest)| (name.clone(), digest.sha256.clone()))
            .collect::<BTreeMap<_, _>>();
        require_same(
            case.case.label(),
            "exact non-node binary set",
            &mut canonical_tool_binaries,
            tool_binaries,
        )?;
        require_same(
            case.case.label(),
            "Gramine image identity",
            &mut canonical_gramine_image,
            scenario.environment.gramine_image_id.clone(),
        )?;
        require_same(
            case.case.label(),
            "OCOMP launch identity",
            &mut canonical_launch_identity,
            scenario.ocomp.topology.launch_identity.clone(),
        )?;
        require_same(
            case.case.label(),
            "canonical genesis digest",
            &mut canonical_genesis,
            scenario.metadosis_p0.genesis.sha256.clone(),
        )?;

        let observation = scenario.ocomp.public_path.capacity_public_path;
        validate_observation(case.case.label(), &observation)?;
        require_same(
            case.case.label(),
            "consensus observation",
            &mut canonical,
            observation,
        )?;

        case_evidence.push(MetadosisP0CaseEvidenceV1 {
            case: case.case,
            scenario_path: scenario_path.display().to_string(),
            scenario_sha256,
            scenario_data_dir: scenario.scenario_data_dir,
            node_binary_sha256: binary.sha256.clone(),
            genesis_sha256: scenario.metadosis_p0.genesis.sha256,
            environment: scenario.metadosis_p0.environment,
        });
    }

    Ok(MetadosisP0ParityEvidenceV1 {
        kind: METADOSIS_P0_EVIDENCE_KIND,
        schema_version: METADOSIS_P0_SCHEMA_VERSION,
        source_revision: expected_source_revision.to_owned(),
        feature_tree_sha256: sha256_hex(feature_tree_bytes),
        canonical_final_fixture_sha256: expected_final_fixture_sha256.to_owned(),
        canonical_genesis_sha256: canonical_genesis.expect("six cases set canonical genesis"),
        gramine_image_id: canonical_gramine_image.expect("six cases set Gramine image"),
        ocomp_launch_identity: canonical_launch_identity
            .expect("six cases set OCOMP launch identity"),
        exact_tool_binaries_sha256: canonical_tool_binaries
            .expect("six cases set exact tool binaries"),
        debug_node_sha256,
        release_node_sha256,
        cases: case_evidence,
        consensus: canonical.expect("six cases set canonical observation"),
    })
}

fn validate_scenario_envelope(
    case: MetadosisP0Case,
    expected_source_revision: &str,
    expected_final_fixture_sha256: &str,
    scenario: &ScenarioRecordV1,
) -> Result<()> {
    ensure!(
        scenario.source.sha == expected_source_revision
            && !scenario.source.tracked_dirty
            && !scenario.source.untracked_dirty,
        "{} is not bound to the clean verifier source revision",
        case.label()
    );
    ensure!(
        scenario.feature == METADOSIS_P0_FEATURE
            && scenario.scenario == METADOSIS_P0_SCENARIO
            && scenario.scenario_id > 0
            && scenario.result == "passed",
        "{} is not a passed exact P0 scenario",
        case.label()
    );
    ensure!(
        scenario.environment.validators == 4
            && scenario.environment.tee == "gramine-direct"
            && scenario.environment.all
            && !scenario.environment.gramine_image_id.is_empty(),
        "{} did not run in the required four-validator Gramine environment",
        case.label()
    );
    ensure!(
        scenario.metadosis_p0.environment == MetadosisP0EnvironmentReceiptV1::expected(case),
        "{} retained the wrong process-environment receipt",
        case.label()
    );
    ensure!(
        scenario.metadosis_p0.canonical_final_fixture_sha256 == expected_final_fixture_sha256,
        "{} used a different canonical Final fixture",
        case.label()
    );
    validate_member_digest(&scenario.metadosis_p0.genesis)?;
    ensure!(
        invocation_has_case(&scenario.invocation, case),
        "{} invocation is not bound to its P0 case",
        case.label()
    );
    let observed_names = scenario
        .ocomp
        .exact_binaries
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let required_names = REQUIRED_BINARY_NAMES.into_iter().collect::<BTreeSet<_>>();
    ensure!(
        observed_names == required_names,
        "{} exact binary set is incomplete or contains unexpected members",
        case.label()
    );
    for digest in scenario.ocomp.exact_binaries.values() {
        validate_member_digest(digest)?;
    }
    validate_launch_identity(&scenario.ocomp.topology.launch_identity)?;
    Ok(())
}

fn validate_observation(
    label: &str,
    observation: &MetadosisP0ConsensusObservationV1,
) -> Result<()> {
    ensure!(
        observation.q_forming_receipt_success
            && is_sha256(&observation.q_forming_receipt_sha256)
            && observation.q_forming_validator_receipt_sha256.len() == 4
            && observation
                .q_forming_validator_receipt_sha256
                .iter()
                .all(|digest| digest == &observation.q_forming_receipt_sha256),
        "{label} q-forming receipt failed or has no four-validator exact digest"
    );
    ensure!(
        observation.canonical_import_verified
            && observation.canonical_import_validator_count == 4
            && observation.q_forming_validator_commitments.len() == 4,
        "{label} did not prove four-validator canonical import"
    );
    let canonical = observation
        .q_forming_validator_commitments
        .first()
        .expect("validated four commitments");
    ensure!(
        observation
            .q_forming_validator_commitments
            .iter()
            .all(|commitment| commitment == canonical)
            && canonical.block_hash == observation.q_forming_block_hash
            && canonical.state_root == observation.q_forming_state_root
            && canonical.ce_root == observation.q_forming_ce_root,
        "{label} validator commitments do not match the retained canonical tuple"
    );
    ensure!(
        observation.q_forming_state_root != B256::ZERO
            && observation.q_forming_ce_root != B256::ZERO,
        "{label} retained an empty state or CE root"
    );
    ensure!(
        observation.tribute_count == 257
            && observation.nod_count == 257
            && observation.worker_shard_count == 2,
        "{label} is not the S+1 maximum-shaped public fixture"
    );
    Ok(())
}

fn validate_feature_tree(bytes: &[u8]) -> Result<()> {
    ensure!(!bytes.is_empty(), "outbe-chain feature tree is empty");
    let text = std::str::from_utf8(bytes).wrap_err("feature tree is not UTF-8")?;
    let lines = text.lines().collect::<Vec<_>>();
    ensure!(
        lines
            .first()
            .is_some_and(|line| line.starts_with("outbe-metadosis v")),
        "feature tree is not rooted at outbe-metadosis"
    );
    ensure!(
        lines.iter().any(|line| line.contains("outbe-chain v")),
        "feature tree does not prove an outbe-chain dependency on outbe-metadosis"
    );
    ensure!(
        !lines
            .iter()
            .any(|line| line.contains("outbe-metadosis feature \"test-utils\"")),
        "outbe-chain normal dependency tree enables outbe-metadosis/test-utils"
    );
    Ok(())
}

fn validate_member_digest(digest: &MemberDigestV1) -> Result<()> {
    ensure!(
        digest.length > 0 && is_sha256(&digest.sha256),
        "evidence member digest is empty or malformed"
    );
    Ok(())
}

fn validate_launch_identity(identity: &MetadosisP0LaunchIdentityV1) -> Result<()> {
    ensure!(
        identity.chain_id > 0
            && is_prefixed_hash(&identity.genesis_hash)
            && is_prefixed_hash(&identity.protocol_bundle_hash)
            && is_prefixed_hash(&identity.fork_install_hash)
            && identity.classification == "final"
            && identity.activation_height == 32
            && identity.metadosis_storage_layout_hash
                == crate::world::ocomp::METADOSIS_STORAGE_LAYOUT_V1_HASH_HEX,
        "OCOMP launch identity is malformed"
    );
    Ok(())
}

fn invocation_has_case(invocation: &[String], case: MetadosisP0Case) -> bool {
    invocation
        .windows(2)
        .any(|pair| pair[0] == "--metadosis-p0-case" && pair[1].as_str() == case.cli_value())
}

fn require_same<T: Eq + Clone>(
    label: &str,
    what: &str,
    canonical: &mut Option<T>,
    observed: T,
) -> Result<()> {
    match canonical.as_ref() {
        Some(expected) => ensure!(expected == &observed, "{label} has a different {what}"),
        None => *canonical = Some(observed),
    }
    Ok(())
}

fn load_single_p0_scenario(evidence_dir: &Path) -> Result<(PathBuf, Vec<u8>, ScenarioRecordV1)> {
    let mut matches = Vec::new();
    for entry in WalkDir::new(evidence_dir).follow_links(false) {
        let entry = entry.wrap_err_with(|| format!("walk {}", evidence_dir.display()))?;
        if !entry.file_type().is_file()
            || entry.path().extension().and_then(|value| value.to_str()) != Some("json")
        {
            continue;
        }
        let bytes = std::fs::read(entry.path())
            .wrap_err_with(|| format!("read {}", entry.path().display()))?;
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if value.get("scenario").and_then(serde_json::Value::as_str) != Some(METADOSIS_P0_SCENARIO)
        {
            continue;
        }
        let scenario = serde_json::from_value::<ScenarioRecordV1>(value)
            .wrap_err_with(|| format!("decode exact P0 scenario {}", entry.path().display()))?;
        matches.push((entry.path().to_path_buf(), bytes, scenario));
    }
    ensure!(
        matches.len() == 1,
        "expected exactly one exact P0 scenario under {}, found {}",
        evidence_dir.display(),
        matches.len()
    );
    Ok(matches.pop().expect("checked one match"))
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn is_git_revision(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_prefixed_hash(value: &str) -> bool {
    value.len() == 66
        && value.starts_with("0x")
        && value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use tempfile::TempDir;

    const SOURCE_REVISION: &str = "0123456789012345678901234567890123456789";
    const FEATURE_TREE: &[u8] =
        b"outbe-metadosis v0.1.0 (/repo/metadosis)\n    outbe-chain v0.1.0 (/repo/chain)\n";
    const FIXTURE_SHA256: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn observation() -> serde_json::Value {
        let commitment = json!({
            "block_hash": B256::repeat_byte(4),
            "state_root": B256::repeat_byte(5),
            "ce_root": B256::repeat_byte(6)
        });
        json!({
            "job_id": B256::repeat_byte(1),
            "result_digest": B256::repeat_byte(2),
            "q_forming_transaction_hash": B256::repeat_byte(3),
            "q_forming_block_number": 42,
            "q_forming_block_hash": B256::repeat_byte(4),
            "q_forming_receipt_success": true,
            "q_forming_receipt_sha256": sha256_hex(b"receipt"),
            "q_forming_validator_receipt_sha256": [
                sha256_hex(b"receipt"), sha256_hex(b"receipt"),
                sha256_hex(b"receipt"), sha256_hex(b"receipt")
            ],
            "q_forming_state_root": B256::repeat_byte(5),
            "q_forming_ce_root": B256::repeat_byte(6),
            "q_forming_validator_commitments": [
                commitment.clone(), commitment.clone(), commitment.clone(), commitment
            ],
            "canonical_import_validator_count": 4,
            "canonical_import_verified": true,
            "tribute_count": 257,
            "nod_count": 257,
            "worker_shard_count": 2,
            "transaction_bytes": 10,
            "block_bytes": 20,
            "gas": 30,
            "internal_work": 40
        })
    }

    fn member(path: &str, sha256: &str) -> serde_json::Value {
        json!({"path": path, "length": 1, "sha256": sha256})
    }

    fn fixture() -> (TempDir, PathBuf, PathBuf, Vec<MetadosisP0CaseInput>) {
        let root = tempfile::tempdir().unwrap();
        let debug = root.path().join("debug-node");
        let release = root.path().join("release-node");
        std::fs::write(&debug, b"debug-node").unwrap();
        std::fs::write(&release, b"release-node").unwrap();
        let debug_hash = sha256_hex(b"debug-node");
        let release_hash = sha256_hex(b"release-node");
        let tool_hashes = [
            ("outbe_ocomp", sha256_hex(b"ocomp")),
            ("outbe_cli", sha256_hex(b"cli")),
            ("outbe_keygen", sha256_hex(b"keygen")),
            ("outbe_e2e", sha256_hex(b"e2e")),
            ("outbe_tee_enclave", sha256_hex(b"enclave")),
        ];
        let cases = MetadosisP0Case::ALL
            .into_iter()
            .enumerate()
            .map(|(index, case)| {
                let directory = root.path().join(case.label());
                std::fs::create_dir(&directory).unwrap();
                let binary_hash = if case.uses_debug_node() {
                    &debug_hash
                } else {
                    &release_hash
                };
                let mut binaries = serde_json::Map::new();
                binaries.insert("outbe_chain".to_owned(), member("outbe-chain", binary_hash));
                for (name, hash) in &tool_hashes {
                    binaries.insert((*name).to_owned(), member(name, hash));
                }
                let scenario = json!({
                    "source": {
                        "sha": SOURCE_REVISION,
                        "tracked_dirty": false,
                        "untracked_dirty": false
                    },
                    "invocation": [
                        "outbe-e2e",
                        "--metadosis-p0-case",
                        case.cli_value()
                    ],
                    "feature": METADOSIS_P0_FEATURE,
                    "scenario": METADOSIS_P0_SCENARIO,
                    "scenario_id": 1,
                    "result": "passed",
                    "environment": {
                        "validators": 4,
                        "tee": "gramine-direct",
                        "all": true,
                        "gramine_image_id": "sha256:gramine"
                    },
                    "scenario_data_dir": format!("/run/{}", case.label()),
                    "metadosis_p0": {
                        "environment": MetadosisP0EnvironmentReceiptV1::expected(case),
                        "genesis": member(
                            "genesis.json",
                            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                        ),
                        "canonical_final_fixture_sha256": FIXTURE_SHA256
                    },
                    "ocomp": {
                        "exact_binaries": binaries,
                        "topology": {
                            "launch_identity": {
                                "chain_id": 1,
                                "genesis_hash": format!("0x{}", "11".repeat(32)),
                                "protocol_bundle_hash": format!("0x{}", "22".repeat(32)),
                                "fork_install_hash": format!("0x{}", "33".repeat(32)),
                                "classification": "final",
                                "activation_height": 32,
                                "metadosis_storage_layout_hash":
                                    crate::world::ocomp::METADOSIS_STORAGE_LAYOUT_V1_HASH_HEX
                            }
                        },
                        "public_path": {"capacity_public_path": observation()}
                    },
                    "nonce": index
                });
                std::fs::write(
                    directory.join("scenario.json"),
                    serde_json::to_vec(&scenario).unwrap(),
                )
                .unwrap();
                MetadosisP0CaseInput {
                    case,
                    evidence_dir: directory,
                }
            })
            .collect();
        (root, debug, release, cases)
    }

    fn verify(
        debug: &Path,
        release: &Path,
        cases: &[MetadosisP0CaseInput],
    ) -> Result<MetadosisP0ParityEvidenceV1> {
        verify_metadosis_p0_parity(
            SOURCE_REVISION,
            FEATURE_TREE,
            debug,
            release,
            FIXTURE_SHA256,
            cases,
        )
    }

    fn mutate_case(
        cases: &[MetadosisP0CaseInput],
        index: usize,
        mutation: impl FnOnce(&mut serde_json::Value),
    ) {
        let path = cases[index].evidence_dir.join("scenario.json");
        let mut scenario: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        mutation(&mut scenario);
        std::fs::write(path, serde_json::to_vec(&scenario).unwrap()).unwrap();
    }

    #[test]
    fn six_equal_cases_close_the_p0_parity_contract() {
        let (_root, debug, release, cases) = fixture();
        let evidence = verify(&debug, &release, &cases).unwrap();
        assert_eq!(evidence.cases.len(), 6);
        assert_eq!(evidence.consensus.tribute_count, 257);
        assert_eq!(evidence.consensus.q_forming_validator_commitments.len(), 4);
    }

    #[test]
    fn changed_consensus_or_failed_run_fails_closed() {
        let (_root, debug, release, cases) = fixture();
        mutate_case(&cases, 4, |scenario| {
            scenario["ocomp"]["public_path"]["capacity_public_path"]["q_forming_state_root"] =
                json!(B256::repeat_byte(9));
        });
        assert!(verify(&debug, &release, &cases).is_err());

        let (_root, debug, release, cases) = fixture();
        mutate_case(&cases, 2, |scenario| {
            scenario["result"] = json!("step_failed");
        });
        assert!(verify(&debug, &release, &cases).is_err());
    }

    #[test]
    fn wrong_env_receipt_or_reused_run_fails_closed() {
        let (_root, debug, release, cases) = fixture();
        mutate_case(&cases, 1, |scenario| {
            scenario["metadosis_p0"]["environment"] = json!(
                MetadosisP0EnvironmentReceiptV1::expected(MetadosisP0Case::DebugUnset)
            );
        });
        assert!(verify(&debug, &release, &cases).is_err());

        let (_root, debug, release, mut cases) = fixture();
        cases[1].evidence_dir = cases[0].evidence_dir.clone();
        assert!(verify(&debug, &release, &cases).is_err());
    }

    #[test]
    fn fail_open_feature_trees_are_rejected() {
        let (_root, debug, release, cases) = fixture();
        for tree in [
            b"".as_slice(),
            b"outbe-metadosis v0.1.0\n".as_slice(),
            b"outbe-metadosis v0.1.0\noutbe-metadosis feature \"test-utils\"\noutbe-chain v0.1.0\n"
                .as_slice(),
        ] {
            assert!(verify_metadosis_p0_parity(
                SOURCE_REVISION,
                tree,
                &debug,
                &release,
                FIXTURE_SHA256,
                &cases
            )
            .is_err());
        }
    }

    #[test]
    fn binary_and_source_mismatches_fail_closed() {
        let (_root, debug, release, cases) = fixture();
        mutate_case(&cases, 3, |scenario| {
            scenario["ocomp"]["exact_binaries"]["outbe_chain"]["sha256"] =
                json!(sha256_hex(b"wrong"));
        });
        assert!(verify(&debug, &release, &cases).is_err());

        let (_root, debug, release, cases) = fixture();
        mutate_case(&cases, 0, |scenario| {
            scenario["source"]["tracked_dirty"] = json!(true);
        });
        assert!(verify(&debug, &release, &cases).is_err());
    }

    #[test]
    fn receipt_import_shape_and_revision_fail_closed() {
        type ScenarioMutation = fn(&mut serde_json::Value);
        let mutations: [ScenarioMutation; 5] = [
            |scenario| {
                scenario["ocomp"]["public_path"]["capacity_public_path"]
                    ["q_forming_receipt_success"] = json!(false);
            },
            |scenario| {
                scenario["ocomp"]["public_path"]["capacity_public_path"]
                    ["canonical_import_verified"] = json!(false);
            },
            |scenario| {
                scenario["ocomp"]["public_path"]["capacity_public_path"]
                    ["q_forming_validator_commitments"] = json!([]);
            },
            |scenario| {
                scenario["ocomp"]["public_path"]["capacity_public_path"]["tribute_count"] =
                    json!(256);
            },
            |scenario| {
                scenario["source"]["sha"] = json!("not-a-revision");
            },
        ];
        for mutation in mutations {
            let (_root, debug, release, cases) = fixture();
            mutate_case(&cases, 5, mutation);
            assert!(verify(&debug, &release, &cases).is_err());
        }
    }

    #[test]
    fn duplicate_exact_scenario_and_removed_artifact_string_fail_closed() {
        let (_root, debug, release, cases) = fixture();
        std::fs::copy(
            cases[0].evidence_dir.join("scenario.json"),
            cases[0].evidence_dir.join("duplicate.json"),
        )
        .unwrap();
        assert!(verify(&debug, &release, &cases).is_err());

        let (_root, debug, release, cases) = fixture();
        std::fs::write(&debug, REMOVED_OWNER_FAILPOINT).unwrap();
        assert!(verify(&debug, &release, &cases).is_err());
    }
}
