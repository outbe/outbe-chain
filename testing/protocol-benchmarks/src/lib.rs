//! Unified gas and latency benchmark support for protocol operations.

use std::collections::BTreeMap;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use thiserror::Error;

mod report;
pub mod scenarios;

pub use report::{
    check_gas_baseline, render_text, update_gas_baseline, write_json_atomic, BenchmarkEnvironment,
    BenchmarkRunReport,
};

pub const REPORT_SCHEMA: &str = "outbe.protocol_benchmark@2";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionClass {
    UserTransaction,
    InternalTransition,
    SystemTransaction,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GasLedger {
    UserTransaction,
    SystemVisible,
    SystemInternal,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Single,
    Typical,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CryptoMode {
    None,
    PortableInProcess,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionLayer {
    Marginal,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioFidelity {
    Full,
    PartialStubbed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScenarioMetadata {
    pub id: String,
    pub display_name: String,
    pub execution_class: ExecutionClass,
    pub profile: Profile,
    pub crypto_mode: CryptoMode,
    pub execution_layer: ExecutionLayer,
    pub fidelity: ScenarioFidelity,
}

impl ScenarioMetadata {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        display_name: impl Into<String>,
        execution_class: ExecutionClass,
        profile: Profile,
    ) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            execution_class,
            profile,
            crypto_mode: CryptoMode::None,
            execution_layer: ExecutionLayer::Marginal,
            fidelity: ScenarioFidelity::Full,
        }
    }

    #[must_use]
    pub const fn with_crypto_mode(mut self, crypto_mode: CryptoMode) -> Self {
        self.crypto_mode = crypto_mode;
        self
    }

    #[must_use]
    pub const fn with_execution_layer(mut self, execution_layer: ExecutionLayer) -> Self {
        self.execution_layer = execution_layer;
        self
    }

    #[must_use]
    pub const fn with_fidelity(mut self, fidelity: ScenarioFidelity) -> Self {
        self.fidelity = fidelity;
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GasComponent {
    pub ledger: GasLedger,
    pub key: String,
    pub gas: u64,
    pub operations: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CalldataStats {
    pub bytes: u64,
    pub zero_bytes: u64,
    pub nonzero_bytes: u64,
    pub transaction_base_gas: u64,
    pub zero_byte_gas: u64,
    pub nonzero_byte_gas: u64,
}

impl CalldataStats {
    #[must_use]
    pub fn ethereum(data: &[u8]) -> Self {
        const TRANSACTION_BASE_GAS: u64 = 21_000;
        const ZERO_BYTE_GAS: u64 = 4;
        const NONZERO_BYTE_GAS: u64 = 16;

        let zero_bytes =
            u64::try_from(data.iter().filter(|byte| **byte == 0).count()).unwrap_or(u64::MAX);
        let bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);
        let nonzero_bytes = bytes.saturating_sub(zero_bytes);
        Self {
            bytes,
            zero_bytes,
            nonzero_bytes,
            transaction_base_gas: TRANSACTION_BASE_GAS,
            zero_byte_gas: zero_bytes.saturating_mul(ZERO_BYTE_GAS),
            nonzero_byte_gas: nonzero_bytes.saturating_mul(NONZERO_BYTE_GAS),
        }
    }

    #[must_use]
    pub const fn intrinsic_gas(self) -> u64 {
        self.transaction_base_gas
            .saturating_add(self.zero_byte_gas)
            .saturating_add(self.nonzero_byte_gas)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageOperationKind {
    Read,
    Write,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageTraceEntry {
    pub module: String,
    pub address: String,
    pub slot: String,
    pub operation: StorageOperationKind,
    pub count: u64,
    pub gas: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EventCount {
    pub emitter: String,
    pub event: String,
    pub count: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChildFrameTrace {
    pub label: String,
    pub target: String,
    pub selector: String,
    pub status: String,
    pub gas_used: u64,
    pub fidelity: ChildFrameFidelity,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildFrameFidelity {
    Production,
    BenchmarkStub,
}

impl GasComponent {
    #[must_use]
    pub fn new(ledger: GasLedger, key: impl Into<String>, gas: u64, operations: u64) -> Self {
        Self {
            ledger,
            key: key.into(),
            gas,
            operations,
            module: None,
        }
    }

    #[must_use]
    pub fn attributed_to(mut self, module: impl Into<String>) -> Self {
        self.module = Some(module.into());
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Observation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_latency_ns: Option<u64>,
    #[serde(default)]
    pub latency_ns: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub setup_components_ns: BTreeMap<String, u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calldata: Option<CalldataStats>,
    pub gas_totals: BTreeMap<GasLedger, u64>,
    pub gas_components: Vec<GasComponent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub storage: Vec<StorageTraceEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventCount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_frames: Vec<ChildFrameTrace>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub postconditions: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifacts: BTreeMap<String, String>,
}

impl Observation {
    #[must_use]
    pub fn new(
        gas_totals: impl IntoIterator<Item = (GasLedger, u64)>,
        gas_components: impl IntoIterator<Item = GasComponent>,
    ) -> Self {
        Self {
            total_latency_ns: None,
            latency_ns: BTreeMap::new(),
            setup_components_ns: BTreeMap::new(),
            calldata: None,
            gas_totals: gas_totals.into_iter().collect(),
            gas_components: gas_components.into_iter().collect(),
            storage: Vec::new(),
            events: Vec::new(),
            child_frames: Vec::new(),
            postconditions: BTreeMap::new(),
            artifacts: BTreeMap::new(),
        }
    }

    #[must_use]
    pub const fn with_total_latency(mut self, latency_ns: u64) -> Self {
        self.total_latency_ns = Some(latency_ns);
        self
    }

    #[must_use]
    pub fn with_latency(mut self, component: impl Into<String>, latency_ns: u64) -> Self {
        self.latency_ns.insert(component.into(), latency_ns);
        self
    }

    #[must_use]
    pub fn with_setup_latency(mut self, component: impl Into<String>, latency_ns: u64) -> Self {
        self.setup_components_ns
            .insert(component.into(), latency_ns);
        self
    }

    #[must_use]
    pub const fn with_calldata(mut self, calldata: CalldataStats) -> Self {
        self.calldata = Some(calldata);
        self
    }

    #[must_use]
    pub fn with_postcondition(
        mut self,
        key: impl Into<String>,
        evidence: impl Into<String>,
    ) -> Self {
        self.postconditions.insert(key.into(), evidence.into());
        self
    }

    #[must_use]
    pub fn with_artifact(mut self, name: impl Into<String>, digest: impl Into<String>) -> Self {
        self.artifacts.insert(name.into(), digest.into());
        self
    }

    fn deterministic_eq(&self, other: &Self) -> bool {
        self.calldata == other.calldata
            && self.gas_totals == other.gas_totals
            && self.gas_components == other.gas_components
            && self.storage == other.storage
            && self.events == other.events
            && self.child_frames == other.child_frames
            && self.postconditions == other.postconditions
            && self.artifacts == other.artifacts
    }
}

pub trait BenchmarkScenario {
    type Prepared;

    fn metadata(&self) -> ScenarioMetadata;
    fn prepare(&self, profile: Profile) -> Result<Self::Prepared, String>;
    fn run_once(&self, prepared: &Self::Prepared) -> Result<Observation, String>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RunConfig {
    pub samples: usize,
    pub warmups: usize,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            samples: 30,
            warmups: 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LatencyStats {
    pub min: u64,
    pub median: u64,
    pub p95: u64,
    pub max: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScenarioReport {
    pub schema: String,
    pub metadata: ScenarioMetadata,
    pub samples: usize,
    pub setup_latency_ns: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub setup_components_ns: BTreeMap<String, u64>,
    pub total_latency_ns: LatencyStats,
    pub component_latency_ns: BTreeMap<String, LatencyStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calldata: Option<CalldataStats>,
    pub gas_totals: BTreeMap<GasLedger, u64>,
    pub gas_components: Vec<GasComponent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub storage: Vec<StorageTraceEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventCount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_frames: Vec<ChildFrameTrace>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub postconditions: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifacts: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GasBaseline {
    pub schema: String,
    pub fixture_version: u32,
    pub scenarios: BTreeMap<String, ScenarioGasBaseline>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScenarioGasBaseline {
    pub metadata: ScenarioMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calldata: Option<CalldataStats>,
    pub gas_totals: BTreeMap<GasLedger, u64>,
    pub gas_components: Vec<GasComponent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub storage: Vec<StorageTraceEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventCount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_frames: Vec<ChildFrameTrace>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifacts: BTreeMap<String, String>,
}

impl GasBaseline {
    pub fn from_reports(
        fixture_version: u32,
        reports: &[ScenarioReport],
    ) -> Result<Self, BenchmarkError> {
        let mut scenarios = BTreeMap::new();
        for report in reports {
            if report.schema != REPORT_SCHEMA {
                return Err(BenchmarkError::Schema {
                    expected: REPORT_SCHEMA.to_owned(),
                    actual: report.schema.clone(),
                });
            }
            let id = report.metadata.id.clone();
            let baseline = ScenarioGasBaseline {
                metadata: report.metadata.clone(),
                calldata: report.calldata,
                gas_totals: report.gas_totals.clone(),
                gas_components: report.gas_components.clone(),
                storage: report.storage.clone(),
                events: report.events.clone(),
                child_frames: report.child_frames.clone(),
                artifacts: report.artifacts.clone(),
            };
            if scenarios.insert(id.clone(), baseline).is_some() {
                return Err(BenchmarkError::DuplicateScenario(id));
            }
        }
        Ok(Self {
            schema: REPORT_SCHEMA.to_owned(),
            fixture_version,
            scenarios,
        })
    }
}

#[derive(Debug, Error)]
pub enum BenchmarkError {
    #[error("sample count must be at least 3, got {0}")]
    InvalidSampleCount(usize),
    #[error("scenario {scenario} prepare failed: {detail}")]
    Prepare { scenario: String, detail: String },
    #[error("scenario {scenario} warmup {warmup} failed: {detail}")]
    Warmup {
        scenario: String,
        warmup: usize,
        detail: String,
    },
    #[error("scenario {scenario} sample {sample} failed: {detail}")]
    Sample {
        scenario: String,
        sample: usize,
        detail: String,
    },
    #[error(
        "scenario {scenario} gas ledger {ledger:?} does not conserve: components={components}, total={total}"
    )]
    GasConservation {
        scenario: String,
        ledger: GasLedger,
        components: u64,
        total: u64,
    },
    #[error("scenario {scenario} deterministic gas changed between samples")]
    GasDrift { scenario: String },
    #[error("scenario {scenario} returned no postcondition evidence")]
    MissingPostcondition { scenario: String },
    #[error("benchmark schema mismatch: expected {expected}, got {actual}")]
    Schema { expected: String, actual: String },
    #[error("duplicate benchmark scenario id {0}")]
    DuplicateScenario(String),
    #[error("benchmark I/O failed for {path}: {detail}")]
    Io { path: String, detail: String },
    #[error("benchmark JSON failed for {path}: {detail}")]
    Json { path: String, detail: String },
    #[error("gas baseline differs from {path}")]
    BaselineMismatch { path: String },
}

pub fn run_scenario<S: BenchmarkScenario>(
    scenario: &S,
    config: RunConfig,
) -> Result<ScenarioReport, BenchmarkError> {
    if config.samples < 3 {
        return Err(BenchmarkError::InvalidSampleCount(config.samples));
    }

    let metadata = scenario.metadata();
    let setup_started = Instant::now();
    let prepared =
        scenario
            .prepare(metadata.profile)
            .map_err(|detail| BenchmarkError::Prepare {
                scenario: metadata.id.clone(),
                detail,
            })?;
    let setup_latency_ns = elapsed_ns(setup_started);

    for warmup in 0..config.warmups {
        let observation =
            scenario
                .run_once(&prepared)
                .map_err(|detail| BenchmarkError::Warmup {
                    scenario: metadata.id.clone(),
                    warmup,
                    detail,
                })?;
        validate_gas(&metadata.id, &observation)?;
    }

    let mut latencies = Vec::with_capacity(config.samples);
    let mut component_latencies: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    let mut canonical = None;
    for sample in 0..config.samples {
        let started = Instant::now();
        let observation =
            scenario
                .run_once(&prepared)
                .map_err(|detail| BenchmarkError::Sample {
                    scenario: metadata.id.clone(),
                    sample,
                    detail,
                })?;
        let wrapper_latency = elapsed_ns(started);
        latencies.push(observation.total_latency_ns.unwrap_or(wrapper_latency));
        validate_gas(&metadata.id, &observation)?;
        for (component, latency_ns) in &observation.latency_ns {
            component_latencies
                .entry(component.clone())
                .or_default()
                .push(*latency_ns);
        }
        match &canonical {
            None => canonical = Some(observation),
            Some(expected) if expected.deterministic_eq(&observation) => {}
            Some(_) => {
                return Err(BenchmarkError::GasDrift {
                    scenario: metadata.id.clone(),
                });
            }
        }
    }
    let canonical = canonical.expect("minimum sample count guarantees one observation");

    Ok(ScenarioReport {
        schema: REPORT_SCHEMA.to_owned(),
        metadata,
        samples: config.samples,
        setup_latency_ns,
        setup_components_ns: canonical.setup_components_ns,
        total_latency_ns: latency_stats(latencies),
        component_latency_ns: component_latencies
            .into_iter()
            .map(|(component, latencies)| (component, latency_stats(latencies)))
            .collect(),
        calldata: canonical.calldata,
        gas_totals: canonical.gas_totals,
        gas_components: canonical.gas_components,
        storage: canonical.storage,
        events: canonical.events,
        child_frames: canonical.child_frames,
        postconditions: canonical.postconditions,
        artifacts: canonical.artifacts,
    })
}

fn validate_gas(scenario: &str, observation: &Observation) -> Result<(), BenchmarkError> {
    if observation.postconditions.is_empty() {
        return Err(BenchmarkError::MissingPostcondition {
            scenario: scenario.to_owned(),
        });
    }
    for (ledger, total) in &observation.gas_totals {
        let components = observation
            .gas_components
            .iter()
            .filter(|component| component.ledger == *ledger)
            .try_fold(0_u64, |sum, component| sum.checked_add(component.gas))
            .unwrap_or(u64::MAX);
        if components != *total {
            return Err(BenchmarkError::GasConservation {
                scenario: scenario.to_owned(),
                ledger: *ledger,
                components,
                total: *total,
            });
        }
    }
    Ok(())
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn latency_stats(mut values: Vec<u64>) -> LatencyStats {
    values.sort_unstable();
    let median = if values.len().is_multiple_of(2) {
        values[values.len() / 2 - 1].saturating_add(values[values.len() / 2]) / 2
    } else {
        values[values.len() / 2]
    };
    let p95_index = values
        .len()
        .saturating_mul(95)
        .div_ceil(100)
        .saturating_sub(1)
        .min(values.len() - 1);
    LatencyStats {
        min: values[0],
        median,
        p95: values[p95_index],
        max: values[values.len() - 1],
    }
}
