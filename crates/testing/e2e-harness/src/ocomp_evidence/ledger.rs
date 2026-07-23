//! Strict parser and reference validator for the checked-in planning ledger.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use eyre::{bail, ensure, Result as EyreResult, WrapErr};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_yaml_ng::Value;

/// Exact ledger kind accepted by this implementation.
pub const LEDGER_KIND: &str = "outbe_ocomp_poc_planning_ledger";

/// Parsed subset of the planning ledger needed by discovery and verification.
#[derive(Clone, Debug, Deserialize)]
pub struct PlanningLedger {
    /// Ledger schema version.
    pub schema_version: u32,
    /// Stable ledger discriminator.
    pub kind: String,
    /// Planning status; runtime evidence must not reinterpret it as PASS.
    pub status: String,
    /// The only normative requirements allowed to remain deferred.
    pub deferred: BTreeMap<String, DeferredRequirement>,
    /// Registered CI lanes.
    pub lanes: BTreeMap<String, LaneDefinition>,
    /// Closed oracle/status policy.
    pub oracle_policy: OraclePolicy,
    /// Required runtime manifest shape.
    pub runtime_evidence: RuntimeEvidencePolicy,
    /// Stable test catalog.
    pub tests: BTreeMap<String, PlannedTest>,
    /// Exactly one closing task per stable test.
    pub task_ownership: BTreeMap<String, Vec<String>>,
    /// Bidirectional normative coverage.
    pub requirements: RequirementCatalog,
}

/// Frozen reason for a deferred requirement.
#[derive(Clone, Debug, Deserialize)]
pub struct DeferredRequirement {
    /// Must be `DEFERRED`.
    pub status: String,
    /// Human-readable, non-empty frozen reason.
    pub reason: String,
}

/// One registered execution lane.
#[derive(Clone, Debug, Deserialize)]
pub struct LaneDefinition {
    /// Stable command surface.
    pub command: String,
    /// Whether closure requires this lane.
    pub required: bool,
    /// Automatic retry count; PoC requires zero.
    pub automatic_retries: u32,
}

/// Oracle and assertion-status allowlists.
#[derive(Clone, Debug, Deserialize)]
pub struct OraclePolicy {
    /// Closed oracle identifiers.
    pub allowed: Vec<String>,
    /// Statuses that can prove a mandatory assertion.
    pub mandatory_statuses: Vec<String>,
    /// Statuses that never prove a mandatory assertion.
    pub non_proving_statuses: Vec<String>,
}

/// Required run-manifest members and assertion fields.
#[derive(Clone, Debug, Deserialize)]
pub struct RuntimeEvidencePolicy {
    /// Runtime schema version.
    pub schema_version: u32,
    /// Manifest filename published last.
    pub manifest_name: String,
    /// Whether publication order is normative.
    pub publish_last: bool,
    /// Required named manifest sections.
    pub required_sections: Vec<String>,
    /// Required assertion fields.
    pub assertion_required_fields: Vec<String>,
    /// Content classes forbidden from retained evidence.
    pub forbidden_content: Vec<String>,
}

/// One stable test declared before its implementation exists.
#[derive(Clone, Debug, Deserialize)]
pub struct PlannedTest {
    /// Must remain `planned` until runtime evidence exists.
    pub status: String,
    /// Evidence layer.
    pub layer: String,
    /// Registered lane.
    pub lane: String,
    /// Repository-relative implementation path.
    pub planned_path: String,
    /// Production boundary the test must exercise.
    pub production_entrypoint: String,
    /// Real components required by the test.
    pub real_components: Vec<String>,
    /// Explicit substitutions, if any.
    #[serde(default)]
    pub substitutions: Vec<String>,
    /// Why the substitutions are legitimate.
    pub substitution_justification: Option<String>,
    /// Higher-layer tests discharging substitutions.
    #[serde(default)]
    pub substitution_discharge: Vec<String>,
    /// Allowed oracle subset for this test.
    pub oracles: Vec<String>,
    /// Required retained evidence names.
    pub evidence: Vec<String>,
}

/// Requirement maps with their intentionally different normative shapes.
#[derive(Clone, Debug, Deserialize)]
pub struct RequirementCatalog {
    /// Verifier-level requirements.
    pub meta: BTreeMap<String, DescribedRequirement>,
    /// ADR invariant coverage.
    pub adr: BTreeMap<String, DescribedRequirement>,
    /// PoC checklist coverage.
    pub poc: BTreeMap<String, Vec<String>>,
    /// Protocol-flow coverage and deferrals.
    pub pfs: BTreeMap<String, PfsRequirement>,
    /// Thirteen-step story coverage.
    pub story: BTreeMap<String, StoryRequirement>,
}

/// Requirement with explanatory text.
#[derive(Clone, Debug, Deserialize)]
pub struct DescribedRequirement {
    /// Normative summary.
    pub text: String,
    /// Stable tests proving the requirement.
    pub tests: Vec<String>,
}

/// PFS row that is required or explicitly deferred.
#[derive(Clone, Debug, Deserialize)]
pub struct PfsRequirement {
    /// `required` or `DEFERRED`.
    pub status: String,
    /// Stable tests; empty only for an allowed deferral.
    pub tests: Vec<String>,
}

/// One story step and its primary oracle.
#[derive(Clone, Debug, Deserialize)]
pub struct StoryRequirement {
    /// Stable tests contributing to the step.
    pub tests: Vec<String>,
    /// Primary allowed oracle.
    pub oracle: String,
}

impl PlanningLedger {
    /// Parse YAML while rejecting duplicate mapping keys at every depth.
    pub fn parse(path: &Path) -> EyreResult<Self> {
        let bytes = std::fs::read(path)
            .wrap_err_with(|| format!("read planning ledger {}", path.display()))?;
        let text = std::str::from_utf8(&bytes).wrap_err("planning ledger is not UTF-8")?;

        let duplicate_check = serde_yaml_ng::Deserializer::from_str(text);
        NoDuplicateValue::deserialize(duplicate_check)
            .wrap_err("planning ledger contains invalid or duplicate YAML keys")?;

        let ledger: Self = serde_yaml_ng::from_str(text)
            .wrap_err_with(|| format!("decode planning ledger {}", path.display()))?;
        ledger.validate()?;
        Ok(ledger)
    }

    /// Validate IDs, ownership, references, lanes, oracles and exact deferrals.
    pub fn validate(&self) -> EyreResult<()> {
        ensure!(self.schema_version == 1, "unsupported ledger schema");
        ensure!(self.kind == LEDGER_KIND, "unexpected ledger kind");
        ensure!(self.status == "planned", "ledger must remain planned");
        ensure!(!self.tests.is_empty(), "ledger has no stable tests");
        ensure!(
            self.runtime_evidence.schema_version == 1,
            "unsupported runtime evidence schema"
        );
        ensure!(
            self.runtime_evidence.manifest_name == "run-manifest.json",
            "runtime manifest name drifted"
        );
        ensure!(
            self.runtime_evidence.publish_last,
            "run manifest must publish last"
        );

        let allowed_oracles =
            unique_nonempty("oracle_policy.allowed", &self.oracle_policy.allowed)?;
        ensure!(
            self.oracle_policy.mandatory_statuses == ["PASS"],
            "PASS must be the only mandatory proving status"
        );
        let non_proving = unique_nonempty(
            "oracle_policy.non_proving_statuses",
            &self.oracle_policy.non_proving_statuses,
        )?;
        for expected in [
            "FAIL",
            "INFRA_ERROR",
            "TIMEOUT",
            "SKIPPED",
            "MISSING",
            "DEFERRED",
        ] {
            ensure!(
                non_proving.contains(expected),
                "non-proving status {expected} is missing"
            );
        }

        for (lane_id, lane) in &self.lanes {
            ensure!(!lane_id.trim().is_empty(), "empty lane ID");
            ensure!(
                !lane.command.trim().is_empty(),
                "lane {lane_id} has no command"
            );
            ensure!(lane.required, "lane {lane_id} must be required");
            ensure!(
                lane.automatic_retries == 0,
                "lane {lane_id} enables automatic retries"
            );
        }

        for (test_id, test) in &self.tests {
            ensure!(valid_test_id(test_id), "invalid stable test ID {test_id}");
            ensure!(test.status == "planned", "test {test_id} is not planned");
            ensure!(!test.layer.trim().is_empty(), "test {test_id} has no layer");
            ensure!(
                self.lanes.contains_key(&test.lane),
                "test {test_id} references unknown lane {}",
                test.lane
            );
            validate_relative_path(&test.planned_path)
                .wrap_err_with(|| format!("invalid planned path for {test_id}"))?;
            ensure!(
                !test.production_entrypoint.trim().is_empty(),
                "test {test_id} has no production entrypoint"
            );
            ensure!(
                !test.real_components.is_empty(),
                "test {test_id} has no real components"
            );
            ensure!(!test.oracles.is_empty(), "test {test_id} has no oracle");
            for oracle in &test.oracles {
                ensure!(
                    allowed_oracles.contains(oracle.as_str()),
                    "test {test_id} uses unknown oracle {oracle}"
                );
            }
            ensure!(!test.evidence.is_empty(), "test {test_id} has no evidence");
            if test.substitutions.is_empty() {
                ensure!(
                    test.substitution_justification.is_none(),
                    "test {test_id} justifies no substitution"
                );
                ensure!(
                    test.substitution_discharge.is_empty(),
                    "test {test_id} discharges no substitution"
                );
            } else {
                ensure!(
                    test.substitution_justification
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty()),
                    "test {test_id} substitution has no justification"
                );
            }
        }

        let test_ids = self.tests.keys().cloned().collect::<BTreeSet<_>>();
        let mut owners = BTreeMap::<String, Vec<String>>::new();
        for expected in 0..=27 {
            let task_id = format!("OCM-{expected:02}");
            ensure!(
                self.task_ownership.contains_key(&task_id),
                "missing task ownership row {task_id}"
            );
        }
        for (task_id, tests) in &self.task_ownership {
            ensure!(valid_task_id(task_id), "invalid task ID {task_id}");
            for test_id in tests {
                ensure!(
                    test_ids.contains(test_id),
                    "task {task_id} references unknown test {test_id}"
                );
                owners
                    .entry(test_id.clone())
                    .or_default()
                    .push(task_id.clone());
            }
        }
        for test_id in &test_ids {
            let test_owners = owners.get(test_id).map(Vec::as_slice).unwrap_or_default();
            ensure!(
                test_owners.len() == 1,
                "test {test_id} must have exactly one owner, got {test_owners:?}"
            );
        }

        for (test_id, test) in &self.tests {
            for discharge in &test.substitution_discharge {
                ensure!(
                    test_ids.contains(discharge),
                    "test {test_id} has unknown substitution discharge {discharge}"
                );
            }
        }

        for (id, requirement) in &self.requirements.meta {
            ensure!(!requirement.text.trim().is_empty(), "{id} has empty text");
            validate_test_refs(id, &requirement.tests, &test_ids, false)?;
        }
        for (id, requirement) in &self.requirements.adr {
            ensure!(!requirement.text.trim().is_empty(), "{id} has empty text");
            validate_test_refs(id, &requirement.tests, &test_ids, false)?;
        }
        for (id, tests) in &self.requirements.poc {
            validate_test_refs(id, tests, &test_ids, false)?;
        }

        let expected_deferred = BTreeSet::from(["PFS-002-07".to_owned(), "PFS-002-08".to_owned()]);
        ensure!(
            self.deferred.keys().cloned().collect::<BTreeSet<_>>() == expected_deferred,
            "only PFS-002-07/-08 may be deferred"
        );
        for (id, deferred) in &self.deferred {
            ensure!(deferred.status == "DEFERRED", "{id} status drifted");
            ensure!(!deferred.reason.trim().is_empty(), "{id} has no reason");
        }
        let mut observed_deferred = BTreeSet::new();
        for (id, requirement) in &self.requirements.pfs {
            match requirement.status.as_str() {
                "required" => validate_test_refs(id, &requirement.tests, &test_ids, false)?,
                "DEFERRED" => {
                    validate_test_refs(id, &requirement.tests, &test_ids, true)?;
                    ensure!(
                        requirement.tests.is_empty(),
                        "deferred requirement {id} owns tests"
                    );
                    observed_deferred.insert(id.clone());
                }
                status => bail!("PFS requirement {id} has illegal status {status}"),
            }
        }
        ensure!(
            observed_deferred == expected_deferred,
            "PFS deferrals do not match the frozen pair"
        );

        for (step, requirement) in &self.requirements.story {
            let step_number = step
                .parse::<u8>()
                .wrap_err_with(|| format!("invalid story step {step}"))?;
            ensure!(
                (1..=13).contains(&step_number),
                "story step {step} is outside 1..13"
            );
            validate_test_refs(step, &requirement.tests, &test_ids, false)?;
            ensure!(
                allowed_oracles.contains(requirement.oracle.as_str()),
                "story step {step} uses unknown oracle {}",
                requirement.oracle
            );
        }
        ensure!(
            self.requirements.story.len() == 13,
            "story must contain exactly thirteen steps"
        );

        let required_fields = unique_nonempty(
            "assertion_required_fields",
            &self.runtime_evidence.assertion_required_fields,
        )?;
        for field in [
            "assertion_id",
            "test_id",
            "status",
            "oracle",
            "expected_artifact_refs",
            "actual_artifact_refs",
            "observed_at",
        ] {
            ensure!(
                required_fields.contains(field),
                "assertion field {field} is missing"
            );
        }
        let _ = unique_nonempty(
            "runtime_evidence.required_sections",
            &self.runtime_evidence.required_sections,
        )?;
        let _ = unique_nonempty(
            "runtime_evidence.forbidden_content",
            &self.runtime_evidence.forbidden_content,
        )?;
        Ok(())
    }

    /// All stable test IDs.
    #[must_use]
    pub fn test_ids(&self) -> BTreeSet<String> {
        self.tests.keys().cloned().collect()
    }

    /// Tests closed by one task.
    pub fn owned_tests(&self, task_id: &str) -> EyreResult<&[String]> {
        self.task_ownership
            .get(task_id)
            .map(Vec::as_slice)
            .ok_or_else(|| eyre::eyre!("unknown task {task_id}"))
    }

    /// Registered lane IDs.
    #[must_use]
    pub fn lane_test_ids(&self, lane_id: &str) -> BTreeSet<String> {
        self.tests
            .iter()
            .filter(|(_, test)| test.lane == lane_id)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Every non-deferred requirement and the tests mapped to it.
    #[must_use]
    pub fn required_coverage(&self) -> BTreeMap<String, Vec<String>> {
        let mut coverage = BTreeMap::new();
        coverage.extend(
            self.requirements
                .meta
                .iter()
                .map(|(id, value)| (id.clone(), value.tests.clone())),
        );
        coverage.extend(
            self.requirements
                .adr
                .iter()
                .map(|(id, value)| (id.clone(), value.tests.clone())),
        );
        coverage.extend(self.requirements.poc.clone());
        coverage.extend(
            self.requirements
                .pfs
                .iter()
                .filter(|(_, value)| value.status == "required")
                .map(|(id, value)| (id.clone(), value.tests.clone())),
        );
        coverage.extend(
            self.requirements
                .story
                .iter()
                .map(|(id, value)| (format!("story/{id}"), value.tests.clone())),
        );
        coverage
    }
}

fn unique_nonempty<'a>(name: &str, values: &'a [String]) -> EyreResult<BTreeSet<&'a str>> {
    let mut unique = BTreeSet::new();
    for value in values {
        ensure!(!value.trim().is_empty(), "{name} contains an empty value");
        ensure!(
            unique.insert(value.as_str()),
            "{name} contains duplicate {value}"
        );
    }
    Ok(unique)
}

fn validate_test_refs(
    requirement_id: &str,
    refs: &[String],
    test_ids: &BTreeSet<String>,
    allow_empty: bool,
) -> EyreResult<()> {
    ensure!(
        allow_empty || !refs.is_empty(),
        "requirement {requirement_id} has no tests"
    );
    let mut unique = BTreeSet::new();
    for test_id in refs {
        ensure!(
            test_ids.contains(test_id),
            "requirement {requirement_id} references unknown test {test_id}"
        );
        ensure!(
            unique.insert(test_id),
            "requirement {requirement_id} repeats test {test_id}"
        );
    }
    Ok(())
}

fn validate_relative_path(path: &str) -> EyreResult<()> {
    let path = Path::new(path);
    ensure!(!path.as_os_str().is_empty(), "empty path");
    ensure!(!path.is_absolute(), "absolute path");
    ensure!(
        path.components()
            .all(|component| matches!(component, std::path::Component::Normal(_))),
        "path contains traversal or platform prefix"
    );
    Ok(())
}

fn valid_test_id(id: &str) -> bool {
    let Some((prefix, digits)) = id.rsplit_once('-') else {
        return false;
    };
    prefix.starts_with("OCM-")
        && prefix[4..]
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        && digits.len() == 3
        && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_task_id(id: &str) -> bool {
    id.len() == 6 && id.starts_with("OCM-") && id[4..].bytes().all(|byte| byte.is_ascii_digit())
}

/// Recursive deserialization sink that rejects duplicate YAML mapping keys.
struct NoDuplicateValue;

impl<'de> Deserialize<'de> for NoDuplicateValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(NoDuplicateVisitor)
    }
}

struct NoDuplicateVisitor;

impl<'de> Visitor<'de> for NoDuplicateVisitor {
    type Value = NoDuplicateValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a YAML value without duplicate mapping keys")
    }

    fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue)
    }

    fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue)
    }

    fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue)
    }

    fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue)
    }

    fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue)
    }

    fn visit_string<E>(self, _: String) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        NoDuplicateValue::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element_seed(NoDuplicateSeed)?.is_some() {}
        Ok(NoDuplicateValue)
    }

    fn visit_map<A>(self, mut mapping: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = Vec::<Value>::new();
        while let Some(key) = mapping.next_key::<Value>()? {
            if keys.contains(&key) {
                return Err(de::Error::custom(format!(
                    "duplicate YAML mapping key {key:?}"
                )));
            }
            keys.push(key);
            mapping.next_value_seed(NoDuplicateSeed)?;
        }
        Ok(NoDuplicateValue)
    }
}

struct NoDuplicateSeed;

impl<'de> DeserializeSeed<'de> for NoDuplicateSeed {
    type Value = NoDuplicateValue;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        NoDuplicateValue::deserialize(deserializer)
    }
}
