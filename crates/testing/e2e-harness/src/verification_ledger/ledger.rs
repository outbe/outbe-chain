//! Strict YAML loading and repository-level domain-pack discovery.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};

use eyre::{bail, ensure, Result as EyreResult, WrapErr};
use serde::de::{self, DeserializeOwned, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_yaml_ng::{Mapping, Value};

use super::schema::{
    DomainPack, DomainPackVerificationV1, RequirementDefinitionV1, TestDefinitionV1,
    VerificationLedgerIndex, VERIFICATION_LEDGER_INDEX_KIND, VERIFICATION_LEDGER_SCHEMA_VERSION,
};

/// Parsed index and its strictly validated domain packs.
#[derive(Clone, Debug)]
pub struct VerificationLedger {
    /// Checked-in index.
    pub index: VerificationLedgerIndex,
    /// Domain packs keyed by namespace.
    pub domain_packs: BTreeMap<String, DomainPack>,
}

impl VerificationLedger {
    /// Parse the repository index and every referenced pack.
    pub fn parse(index_path: &Path) -> EyreResult<Self> {
        let index: VerificationLedgerIndex = read_yaml_strict(index_path)?;
        ensure!(
            index.schema_version == VERIFICATION_LEDGER_SCHEMA_VERSION,
            "unsupported verification-ledger index schema"
        );
        ensure!(
            index.kind == VERIFICATION_LEDGER_INDEX_KIND,
            "unexpected verification-ledger index kind"
        );
        ensure!(
            !index.domain_packs.is_empty(),
            "verification-ledger index is empty"
        );

        let root = index_path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| eyre::eyre!("verification-ledger index has no repository root"))?;
        let mut packs = BTreeMap::new();
        let mut all_test_ids = BTreeSet::new();
        let mut all_requirement_ids = BTreeSet::new();

        for entry in &index.domain_packs {
            validate_namespace(&entry.namespace)?;
            ensure!(
                entry.schema_version == VERIFICATION_LEDGER_SCHEMA_VERSION,
                "unsupported domain-pack schema for {}",
                entry.namespace
            );
            let relative = validate_relative_path(&entry.path)?;
            let path = root.join(relative);
            let raw: Value = read_yaml_strict(&path)?;
            let pack = decode_domain_pack(raw)
                .wrap_err_with(|| format!("decode domain pack {}", path.display()))?;
            ensure!(
                pack.verification.schema_version == entry.schema_version,
                "domain-pack schema mismatch for {}",
                entry.namespace
            );
            ensure!(
                pack.verification.namespace == entry.namespace,
                "domain-pack namespace mismatch: index {} != pack {}",
                entry.namespace,
                pack.verification.namespace
            );
            ensure!(
                packs
                    .insert(entry.namespace.clone(), pack.clone())
                    .is_none(),
                "duplicate namespace {}",
                entry.namespace
            );

            for test_id in pack.tests.keys() {
                ensure!(
                    all_test_ids.insert(test_id.clone()),
                    "duplicate test ID {test_id} across domain packs"
                );
            }
            for requirement_id in pack.requirements.keys() {
                ensure!(
                    all_requirement_ids.insert(requirement_id.clone()),
                    "duplicate requirement ID {requirement_id} across domain packs"
                );
            }
        }

        for (namespace, pack) in &packs {
            validate_pack(namespace, pack, &packs)?;
        }

        Ok(Self {
            index,
            domain_packs: packs,
        })
    }

    /// Resolve one indexed namespace. Unknown namespaces fail closed.
    pub fn domain_pack(&self, namespace: &str) -> EyreResult<&DomainPack> {
        self.domain_packs
            .get(namespace)
            .ok_or_else(|| eyre::eyre!("unknown verification namespace {namespace}"))
    }
}

/// Decode any YAML file while rejecting duplicate keys at every depth.
pub fn read_yaml_strict<T: DeserializeOwned>(path: &Path) -> EyreResult<T> {
    let bytes =
        std::fs::read(path).wrap_err_with(|| format!("read YAML document {}", path.display()))?;
    let text = std::str::from_utf8(&bytes).wrap_err("YAML document is not UTF-8")?;
    let duplicate_check = serde_yaml_ng::Deserializer::from_str(text);
    NoDuplicateValue::deserialize(duplicate_check)
        .wrap_err("YAML document contains invalid or duplicate mapping keys")?;
    serde_yaml_ng::from_str(text).wrap_err_with(|| format!("decode YAML {}", path.display()))
}

fn decode_domain_pack(raw: Value) -> EyreResult<DomainPack> {
    let mapping = raw
        .as_mapping()
        .ok_or_else(|| eyre::eyre!("domain pack must be a YAML mapping"))?;
    let verification_value = mapping_value(mapping, "verification")?.clone();
    let verification: DomainPackVerificationV1 =
        serde_yaml_ng::from_value(verification_value).wrap_err("decode verification header")?;
    let tests_value = mapping_value(mapping, "tests")?.clone();
    let tests: BTreeMap<String, TestDefinitionV1> =
        serde_yaml_ng::from_value(tests_value).wrap_err("decode generic test catalog")?;
    let requirements_value = mapping_value(mapping, "requirements")?;
    let mut requirements = BTreeMap::new();
    extract_requirements(requirements_value, &mut requirements)?;
    ensure!(
        !requirements.is_empty(),
        "domain pack has no generic requirement references"
    );
    Ok(DomainPack {
        raw,
        verification,
        tests,
        requirements,
    })
}

fn mapping_value<'a>(mapping: &'a Mapping, key: &str) -> EyreResult<&'a Value> {
    mapping
        .get(Value::String(key.to_owned()))
        .ok_or_else(|| eyre::eyre!("domain pack is missing {key}"))
}

fn extract_requirements(
    value: &Value,
    requirements: &mut BTreeMap<String, RequirementDefinitionV1>,
) -> EyreResult<()> {
    let mapping = value
        .as_mapping()
        .ok_or_else(|| eyre::eyre!("requirements must be a mapping"))?;
    for (key, entry) in mapping {
        let id = key
            .as_str()
            .ok_or_else(|| eyre::eyre!("requirement ID must be a string"))?;
        if let Some(tests) = string_sequence(entry) {
            insert_requirement(requirements, id, tests, true)?;
            continue;
        }
        let Some(entry_mapping) = entry.as_mapping() else {
            bail!("requirement group {id} must be a mapping or test list");
        };
        if let Some(tests_value) = entry_mapping.get(Value::String("tests".to_owned())) {
            let tests = string_sequence(tests_value)
                .ok_or_else(|| eyre::eyre!("requirement {id} tests must be a string list"))?;
            let required = entry_mapping
                .get(Value::String("status".to_owned()))
                .and_then(Value::as_str)
                .is_none_or(|status| {
                    !matches!(
                        status.to_ascii_uppercase().as_str(),
                        "DEFERRED" | "RETIRED" | "OPTIONAL"
                    )
                });
            insert_requirement(requirements, id, tests, required)?;
        } else {
            extract_requirements(entry, requirements)?;
        }
    }
    Ok(())
}

fn string_sequence(value: &Value) -> Option<Vec<String>> {
    value.as_sequence().and_then(|sequence| {
        sequence
            .iter()
            .map(|item| item.as_str().map(ToOwned::to_owned))
            .collect()
    })
}

fn insert_requirement(
    requirements: &mut BTreeMap<String, RequirementDefinitionV1>,
    id: &str,
    tests: Vec<String>,
    required: bool,
) -> EyreResult<()> {
    ensure!(!id.trim().is_empty(), "empty requirement ID");
    ensure!(
        requirements
            .insert(id.to_owned(), RequirementDefinitionV1 { tests, required })
            .is_none(),
        "duplicate requirement ID {id} inside domain pack"
    );
    Ok(())
}

fn validate_pack(
    namespace: &str,
    pack: &DomainPack,
    packs: &BTreeMap<String, DomainPack>,
) -> EyreResult<()> {
    let identity = &pack.verification.evidence_identity;
    ensure!(
        identity.exact_revision,
        "domain pack {namespace} does not require an exact revision"
    );
    ensure!(
        !identity.artifact_profile.trim().is_empty(),
        "domain pack {namespace} has no artifact profile"
    );
    ensure!(
        !identity.linux_lane.trim().is_empty(),
        "domain pack {namespace} has no Linux lane"
    );
    ensure!(
        !pack.tests.is_empty(),
        "domain pack {namespace} has no tests"
    );

    for (test_id, test) in &pack.tests {
        ensure!(!test_id.trim().is_empty(), "empty test ID in {namespace}");
        ensure!(
            !test.target.trim().is_empty(),
            "test {test_id} has no target"
        );
        ensure!(
            test.name
                .as_ref()
                .is_none_or(|name| !name.trim().is_empty()),
            "test {test_id} has an empty name"
        );
        ensure!(
            !test.production_interface.trim().is_empty(),
            "test {test_id} has no production interface"
        );
        ensure!(!test.lane.trim().is_empty(), "test {test_id} has no lane");
        ensure!(
            test.substitutions
                .iter()
                .all(|substitution| !substitution.trim().is_empty()),
            "test {test_id} has an empty substitution"
        );
        ensure!(
            !(test.required && (test.ignored || test.skipped)),
            "required test {test_id} is ignored or skipped"
        );
    }

    for (requirement_id, requirement) in &pack.requirements {
        ensure!(
            !requirement.required || !requirement.tests.is_empty(),
            "required requirement {requirement_id} has no tests"
        );
        for reference in &requirement.tests {
            let (reference_namespace, test_id) = split_reference(namespace, reference, packs)?;
            ensure!(
                reference_namespace == namespace,
                "cross-pack requirement reference {reference} from {namespace}"
            );
            ensure!(
                pack.tests.contains_key(test_id),
                "requirement {requirement_id} references unknown test {reference}"
            );
        }
    }
    Ok(())
}

fn split_reference<'a>(
    local_namespace: &'a str,
    reference: &'a str,
    packs: &BTreeMap<String, DomainPack>,
) -> EyreResult<(&'a str, &'a str)> {
    if let Some((namespace, id)) = reference.split_once("::") {
        ensure!(
            packs.contains_key(namespace),
            "unknown namespace {namespace} in reference {reference}"
        );
        ensure!(!id.is_empty(), "empty test ID in reference {reference}");
        Ok((namespace, id))
    } else {
        Ok((local_namespace, reference))
    }
}

fn validate_namespace(namespace: &str) -> EyreResult<()> {
    ensure!(!namespace.is_empty(), "empty verification namespace");
    ensure!(
        namespace
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'),
        "invalid verification namespace {namespace}"
    );
    Ok(())
}

fn validate_relative_path(path: &str) -> EyreResult<PathBuf> {
    ensure!(!path.trim().is_empty(), "empty domain-pack path");
    ensure!(!path.contains('\\'), "domain-pack path uses backslashes");
    let candidate = Path::new(path);
    ensure!(
        !candidate.is_absolute(),
        "domain-pack path must be relative"
    );
    ensure!(
        candidate
            .components()
            .all(|component| { matches!(component, Component::Normal(_) | Component::CurDir) }),
        "domain-pack path escapes repository root"
    );
    Ok(candidate.to_owned())
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
