//! Runtime correlation contract for the public Tribute prefix of OCOMP evidence.
//!
//! This builder is deliberately observational: it cannot create a JobIntent,
//! snapshot, manifest, result or state. It only closes a record after the
//! harness has retained a successful public receipt/finality reference and one
//! independently observed Mongo/CE source package per pinned validator.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Public transaction, inclusion and finalized-chain evidence for one Tribute.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicTributeCorrelationV1 {
    pub transaction_hash: String,
    pub receipt_block_number: u64,
    pub receipt_block_hash: String,
    pub finalized_block_number: u64,
    pub finalized_block_hash: String,
    pub owner: String,
    pub worldwide_day: u32,
    pub raw_entity_id: String,
    pub stored_body_sha256: String,
}

/// One validator's independently read and verified projection/CE package.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidatorSourceCorrelationV1 {
    pub validator_index: u8,
    pub finalized_block_number: u64,
    pub finalized_block_hash: String,
    pub raw_entity_id: String,
    pub stored_body_sha256: String,
    pub primary_projection_sha256: String,
    pub owner_index_sha256: String,
    pub worldwide_day_index_sha256: String,
    pub ce_root: String,
    pub ce_proof_sha256: String,
}

/// Production-observed JobIntent/snapshot/manifest bindings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobIntentCorrelationV1 {
    pub intent_id: String,
    pub job_id: String,
    pub finalized_block_number: u64,
    pub finalized_block_hash: String,
    pub snapshot_root: String,
    pub input_manifest_hash: String,
    pub tribute_stream_root: String,
    pub fixture_membership_proof_sha256: String,
}

/// Closed public-Tribute prefix retained before an OCOMP scenario advances.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CorrelatedTributeFixtureV1 {
    pub public_tribute: PublicTributeCorrelationV1,
    pub validator_sources: Vec<ValidatorSourceCorrelationV1>,
    pub job_intent: JobIntentCorrelationV1,
}

/// Fail-closed construction errors for a fixture correlation record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CorrelationError {
    CorrelationClosed,
    PublicTributeAlreadyRecorded,
    PublicTributeMissing,
    InvalidValidatorIndex(u8),
    DuplicateValidator(u8),
    SourceIdentityMismatch {
        validator_index: u8,
        field: &'static str,
    },
    MissingValidatorSources(Vec<u8>),
    JobIntentAlreadyRecorded,
    JobIntentBindingMismatch(&'static str),
    JobIntentMissing,
    EmptyField(&'static str),
    InvalidValidatorCount(usize),
}

impl fmt::Display for CorrelationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CorrelationClosed => {
                formatter.write_str("public Tribute correlation is already closed")
            }
            Self::PublicTributeAlreadyRecorded => {
                formatter.write_str("public Tribute evidence was already recorded")
            }
            Self::PublicTributeMissing => {
                formatter.write_str("public Tribute evidence must be recorded first")
            }
            Self::InvalidValidatorIndex(index) => {
                write!(
                    formatter,
                    "validator index {index} is outside the pinned snapshot"
                )
            }
            Self::DuplicateValidator(index) => {
                write!(
                    formatter,
                    "validator {index} source evidence was already recorded"
                )
            }
            Self::SourceIdentityMismatch {
                validator_index,
                field,
            } => write!(
                formatter,
                "validator {validator_index} source evidence changed shared field {field}"
            ),
            Self::MissingValidatorSources(indices) => {
                write!(
                    formatter,
                    "missing validator source evidence for {indices:?}"
                )
            }
            Self::JobIntentAlreadyRecorded => {
                formatter.write_str("JobIntent evidence was already recorded")
            }
            Self::JobIntentBindingMismatch(field) => {
                write!(
                    formatter,
                    "JobIntent evidence changed source binding {field}"
                )
            }
            Self::JobIntentMissing => formatter.write_str("JobIntent evidence is missing"),
            Self::EmptyField(field) => write!(formatter, "evidence field {field} is empty"),
            Self::InvalidValidatorCount(count) => {
                write!(
                    formatter,
                    "validator source count {count} is outside 1..=256"
                )
            }
        }
    }
}

impl Error for CorrelationError {}

/// Ordered builder enforcing the public ingress → pinned sources → JobIntent path.
#[derive(Clone, Debug)]
pub struct TributeCorrelationBuilder {
    expected_validator_count: usize,
    public_tribute: Option<PublicTributeCorrelationV1>,
    validator_sources: BTreeMap<u8, ValidatorSourceCorrelationV1>,
    job_intent: Option<JobIntentCorrelationV1>,
}

impl TributeCorrelationBuilder {
    pub fn new(expected_validator_count: usize) -> Result<Self, CorrelationError> {
        if !(1..=usize::from(u8::MAX) + 1).contains(&expected_validator_count) {
            return Err(CorrelationError::InvalidValidatorCount(
                expected_validator_count,
            ));
        }
        Ok(Self {
            expected_validator_count,
            public_tribute: None,
            validator_sources: BTreeMap::new(),
            job_intent: None,
        })
    }

    pub fn record_public_tribute(
        &mut self,
        evidence: PublicTributeCorrelationV1,
    ) -> Result<(), CorrelationError> {
        if self.public_tribute.is_some() {
            return Err(CorrelationError::PublicTributeAlreadyRecorded);
        }
        require_public_tribute_fields(&evidence)?;
        self.public_tribute = Some(evidence);
        Ok(())
    }

    pub fn record_validator_source(
        &mut self,
        evidence: ValidatorSourceCorrelationV1,
    ) -> Result<(), CorrelationError> {
        if usize::from(evidence.validator_index) >= self.expected_validator_count {
            return Err(CorrelationError::InvalidValidatorIndex(
                evidence.validator_index,
            ));
        }
        if self
            .validator_sources
            .contains_key(&evidence.validator_index)
        {
            return Err(CorrelationError::DuplicateValidator(
                evidence.validator_index,
            ));
        }
        let public = self
            .public_tribute
            .as_ref()
            .ok_or(CorrelationError::PublicTributeMissing)?;
        require_source_fields(&evidence)?;
        require_equal(
            evidence.validator_index,
            "finalized_block_number",
            evidence.finalized_block_number,
            public.finalized_block_number,
        )?;
        require_equal(
            evidence.validator_index,
            "finalized_block_hash",
            &evidence.finalized_block_hash,
            &public.finalized_block_hash,
        )?;
        require_equal(
            evidence.validator_index,
            "raw_entity_id",
            &evidence.raw_entity_id,
            &public.raw_entity_id,
        )?;
        require_equal(
            evidence.validator_index,
            "stored_body_sha256",
            &evidence.stored_body_sha256,
            &public.stored_body_sha256,
        )?;
        if let Some(first) = self.validator_sources.values().next() {
            for (field, actual, expected) in [
                (
                    "primary_projection_sha256",
                    &evidence.primary_projection_sha256,
                    &first.primary_projection_sha256,
                ),
                (
                    "owner_index_sha256",
                    &evidence.owner_index_sha256,
                    &first.owner_index_sha256,
                ),
                (
                    "worldwide_day_index_sha256",
                    &evidence.worldwide_day_index_sha256,
                    &first.worldwide_day_index_sha256,
                ),
                ("ce_root", &evidence.ce_root, &first.ce_root),
            ] {
                require_equal(evidence.validator_index, field, actual, expected)?;
            }
        }
        self.validator_sources
            .insert(evidence.validator_index, evidence);
        Ok(())
    }

    pub fn record_job_intent(
        &mut self,
        evidence: JobIntentCorrelationV1,
    ) -> Result<(), CorrelationError> {
        if self.job_intent.is_some() {
            return Err(CorrelationError::JobIntentAlreadyRecorded);
        }
        let public = self
            .public_tribute
            .as_ref()
            .ok_or(CorrelationError::PublicTributeMissing)?;
        let missing = self.missing_validator_sources();
        if !missing.is_empty() {
            return Err(CorrelationError::MissingValidatorSources(missing));
        }
        require_job_fields(&evidence)?;
        if evidence.finalized_block_number != public.finalized_block_number {
            return Err(CorrelationError::JobIntentBindingMismatch(
                "finalized_block_number",
            ));
        }
        if evidence.finalized_block_hash != public.finalized_block_hash {
            return Err(CorrelationError::JobIntentBindingMismatch(
                "finalized_block_hash",
            ));
        }
        self.job_intent = Some(evidence);
        Ok(())
    }

    #[must_use]
    pub fn missing_validator_sources(&self) -> Vec<u8> {
        (0..self.expected_validator_count)
            .map(|index| u8::try_from(index).expect("validator source index is bounded"))
            .filter(|index| !self.validator_sources.contains_key(index))
            .collect()
    }

    pub fn finish(self) -> Result<CorrelatedTributeFixtureV1, CorrelationError> {
        let missing = self.missing_validator_sources();
        if !missing.is_empty() {
            return Err(CorrelationError::MissingValidatorSources(missing));
        }
        let public_tribute = self
            .public_tribute
            .ok_or(CorrelationError::PublicTributeMissing)?;
        let job_intent = self.job_intent.ok_or(CorrelationError::JobIntentMissing)?;
        Ok(CorrelatedTributeFixtureV1 {
            public_tribute,
            validator_sources: self.validator_sources.into_values().collect(),
            job_intent,
        })
    }
}

fn require_public_tribute_fields(
    evidence: &PublicTributeCorrelationV1,
) -> Result<(), CorrelationError> {
    for (field, value) in [
        ("transaction_hash", evidence.transaction_hash.as_str()),
        ("receipt_block_hash", evidence.receipt_block_hash.as_str()),
        (
            "finalized_block_hash",
            evidence.finalized_block_hash.as_str(),
        ),
        ("owner", evidence.owner.as_str()),
        ("raw_entity_id", evidence.raw_entity_id.as_str()),
        ("stored_body_sha256", evidence.stored_body_sha256.as_str()),
    ] {
        require_nonempty(field, value)?;
    }
    Ok(())
}

fn require_source_fields(evidence: &ValidatorSourceCorrelationV1) -> Result<(), CorrelationError> {
    for (field, value) in [
        (
            "finalized_block_hash",
            evidence.finalized_block_hash.as_str(),
        ),
        ("raw_entity_id", evidence.raw_entity_id.as_str()),
        ("stored_body_sha256", evidence.stored_body_sha256.as_str()),
        (
            "primary_projection_sha256",
            evidence.primary_projection_sha256.as_str(),
        ),
        ("owner_index_sha256", evidence.owner_index_sha256.as_str()),
        (
            "worldwide_day_index_sha256",
            evidence.worldwide_day_index_sha256.as_str(),
        ),
        ("ce_root", evidence.ce_root.as_str()),
        ("ce_proof_sha256", evidence.ce_proof_sha256.as_str()),
    ] {
        require_nonempty(field, value)?;
    }
    Ok(())
}

fn require_job_fields(evidence: &JobIntentCorrelationV1) -> Result<(), CorrelationError> {
    for (field, value) in [
        ("intent_id", evidence.intent_id.as_str()),
        ("job_id", evidence.job_id.as_str()),
        (
            "finalized_block_hash",
            evidence.finalized_block_hash.as_str(),
        ),
        ("snapshot_root", evidence.snapshot_root.as_str()),
        ("input_manifest_hash", evidence.input_manifest_hash.as_str()),
        ("tribute_stream_root", evidence.tribute_stream_root.as_str()),
        (
            "fixture_membership_proof_sha256",
            evidence.fixture_membership_proof_sha256.as_str(),
        ),
    ] {
        require_nonempty(field, value)?;
    }
    Ok(())
}

fn require_nonempty(field: &'static str, value: &str) -> Result<(), CorrelationError> {
    if value.is_empty() {
        return Err(CorrelationError::EmptyField(field));
    }
    Ok(())
}

fn require_equal<T: PartialEq>(
    validator_index: u8,
    field: &'static str,
    actual: T,
    expected: T,
) -> Result<(), CorrelationError> {
    if actual != expected {
        return Err(CorrelationError::SourceIdentityMismatch {
            validator_index,
            field,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public() -> PublicTributeCorrelationV1 {
        PublicTributeCorrelationV1 {
            transaction_hash: "0xtx".to_owned(),
            receipt_block_number: 31,
            receipt_block_hash: "0xreceipt".to_owned(),
            finalized_block_number: 31,
            finalized_block_hash: "0xfinal".to_owned(),
            owner: "0xowner".to_owned(),
            worldwide_day: 20_260_726,
            raw_entity_id: "entity".to_owned(),
            stored_body_sha256: "body".to_owned(),
        }
    }

    fn source(validator_index: u8) -> ValidatorSourceCorrelationV1 {
        ValidatorSourceCorrelationV1 {
            validator_index,
            finalized_block_number: 31,
            finalized_block_hash: "0xfinal".to_owned(),
            raw_entity_id: "entity".to_owned(),
            stored_body_sha256: "body".to_owned(),
            primary_projection_sha256: "primary".to_owned(),
            owner_index_sha256: "owner-index".to_owned(),
            worldwide_day_index_sha256: "day-index".to_owned(),
            ce_root: "ce-root".to_owned(),
            ce_proof_sha256: format!("ce-proof-{validator_index}"),
        }
    }

    fn job() -> JobIntentCorrelationV1 {
        JobIntentCorrelationV1 {
            intent_id: "intent".to_owned(),
            job_id: "job".to_owned(),
            finalized_block_number: 31,
            finalized_block_hash: "0xfinal".to_owned(),
            snapshot_root: "snapshot".to_owned(),
            input_manifest_hash: "manifest".to_owned(),
            tribute_stream_root: "tribute-root".to_owned(),
            fixture_membership_proof_sha256: "membership-proof".to_owned(),
        }
    }

    #[test]
    fn job_intent_cannot_precede_every_pinned_validator_source() {
        let mut builder = TributeCorrelationBuilder::new(5).unwrap();
        builder.record_public_tribute(public()).unwrap();
        for index in 0..4 {
            builder.record_validator_source(source(index)).unwrap();
        }

        assert_eq!(
            builder.record_job_intent(job()),
            Err(CorrelationError::MissingValidatorSources(vec![4]))
        );
        builder.record_validator_source(source(4)).unwrap();
        builder.record_job_intent(job()).unwrap();

        let fixture = builder.finish().unwrap();
        assert_eq!(fixture.validator_sources.len(), 5);
        assert_eq!(
            fixture
                .validator_sources
                .iter()
                .map(|source| source.validator_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn one_domain_source_mismatch_fails_before_job_binding() {
        let mut builder = TributeCorrelationBuilder::new(4).unwrap();
        builder.record_public_tribute(public()).unwrap();
        builder.record_validator_source(source(0)).unwrap();
        let mut changed = source(1);
        changed.primary_projection_sha256 = "changed".to_owned();

        assert_eq!(
            builder.record_validator_source(changed),
            Err(CorrelationError::SourceIdentityMismatch {
                validator_index: 1,
                field: "primary_projection_sha256",
            })
        );
        assert_eq!(builder.missing_validator_sources(), vec![1, 2, 3]);
    }
}
