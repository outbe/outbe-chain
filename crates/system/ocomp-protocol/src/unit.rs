use alloy_primitives::B256;

use crate::{
    common::{BoundedBytes, EntityId36},
    error::ProtocolError,
    hash::hash_framed,
    registry::HashDomain,
    schema::{
        encode_nested_value, impl_top_level_codec, require, wire_enum_u8, wire_struct, NestedCodec,
        SchemaLimits,
    },
    CanonicalReader, CanonicalWriter,
};

wire_enum_u8! {
    pub enum InputPurpose {
        InputManifest = 1,
        TributeStream = 2,
        FidelityOpenings = 3,
        OracleOpenings = 4,
        EnumeratedTributes = 10,
        FidelityPartials = 11,
        FiFractionTable = 12,
        AmountRecords = 13,
        GratisPrefixTable = 14,
        FinalizedOutputRecords = 15,
        OwnerOrderedRecords = 16,
        BucketOrderedRecords = 17,
        RootSummary = 18,
    }
}

wire_enum_u8! {
    pub enum InputSourceKind {
        AuthenticatedRoot = 1,
        UnitOutput = 2,
        CanonicalEmpty = 3,
    }
}

wire_enum_u8! {
    pub enum UnitPhase {
        Enumerate = 1,
        FidelityMap = 2,
        FixedReduce = 3,
        AmountMap = 4,
        GratisPrefix = 5,
        OutputFinalize = 6,
        OwnerShuffle = 7,
        BucketShuffle = 8,
        RootReduce = 9,
        // Top-down half of the deterministic parallel Gratis prefix scan.
        // `GratisPrefix` keeps its frozen tag as the bottom-up summary phase.
        GratisPrefixDown = 10,
    }
}

wire_struct! {
    pub struct CanonicalInputRefV1 {
        pub purpose: InputPurpose,
        pub source_kind: InputSourceKind,
        pub source_id: B256,
        pub record_count_limit: u32,
        pub max_encoded_bytes: u64,
        pub max_decoded_bytes: u64,
    }
}

wire_struct! {
    pub struct EntityIdHalfOpenRange {
        pub start: EntityId36,
        pub end: Option<EntityId36>,
    }
}

wire_struct! {
    pub struct FidelityIndexHalfOpenRange {
        pub start: u32,
        pub end: u32,
    }
}

wire_struct! {
    pub struct CanonicalRunSpan {
        pub start_run: u32,
        pub end_run: u32,
    }
}

wire_struct! {
    pub struct BinaryReducerNode {
        pub level: u16,
        pub index: u32,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnitInterval {
    EntityIdRange(EntityIdHalfOpenRange),
    FidelityIndexRange(FidelityIndexHalfOpenRange),
    CanonicalRunSpan(CanonicalRunSpan),
    BinaryReducerNode(BinaryReducerNode),
}

impl UnitInterval {
    #[must_use]
    pub const fn tag(&self) -> u8 {
        match self {
            Self::EntityIdRange(_) => 1,
            Self::FidelityIndexRange(_) => 2,
            Self::CanonicalRunSpan(_) => 3,
            Self::BinaryReducerNode(_) => 4,
        }
    }
}

impl NestedCodec for UnitInterval {
    fn validate(&self, limits: &SchemaLimits) -> Result<(), ProtocolError> {
        match self {
            Self::EntityIdRange(value) => value.validate(limits),
            Self::FidelityIndexRange(value) => {
                value.validate(limits)?;
                require(value.start < value.end, "non-empty fidelity range")
            }
            Self::CanonicalRunSpan(value) => {
                value.validate(limits)?;
                require(
                    value.start_run < value.end_run,
                    "non-empty canonical run span",
                )
            }
            Self::BinaryReducerNode(value) => value.validate(limits),
        }
    }

    fn encode_nested(
        &self,
        output: &mut CanonicalWriter,
        limits: &SchemaLimits,
    ) -> Result<(), ProtocolError> {
        output.write_u8(self.tag())?;
        match self {
            Self::EntityIdRange(value) => value.encode_nested(output, limits),
            Self::FidelityIndexRange(value) => value.encode_nested(output, limits),
            Self::CanonicalRunSpan(value) => value.encode_nested(output, limits),
            Self::BinaryReducerNode(value) => value.encode_nested(output, limits),
        }
    }

    fn decode_nested(
        input: &mut CanonicalReader<'_>,
        limits: &SchemaLimits,
    ) -> Result<Self, ProtocolError> {
        match input.read_u8()? {
            1 => Ok(Self::EntityIdRange(EntityIdHalfOpenRange::decode_nested(
                input, limits,
            )?)),
            2 => Ok(Self::FidelityIndexRange(
                FidelityIndexHalfOpenRange::decode_nested(input, limits)?,
            )),
            3 => Ok(Self::CanonicalRunSpan(CanonicalRunSpan::decode_nested(
                input, limits,
            )?)),
            4 => Ok(Self::BinaryReducerNode(BinaryReducerNode::decode_nested(
                input, limits,
            )?)),
            value => Err(ProtocolError::UnknownEnum {
                width: 8,
                value: u16::from(value),
            }),
        }
    }
}

wire_struct! {
    pub struct UnitSpecV1 {
        pub protocol_bundle_hash: B256,
        pub job_id: B256,
        pub attempt: u32,
        pub phase: UnitPhase,
        pub interval: UnitInterval,
        pub canonical_ordered_inputs: Vec<CanonicalInputRefV1>,
        pub lysis_program_semantics_hash: B256,
        pub planner_spec_version: u16,
        pub reducer_spec_version: u16,
    }
    validate = validate_unit_spec;
}
impl_top_level_codec!(UnitSpecV1, UnitSpecV1);

wire_struct! {
    /// Constant-size commitment to a lazily materialized deterministic plan.
    ///
    /// The complete Tribute population is never embedded as a vector here.
    /// Primary unit specifications are committed by an ordered root and may be
    /// generated or verified independently by ordinal.
    pub struct PlanCommitmentV1 {
        pub protocol_bundle_hash: B256,
        pub job_id: B256,
        pub attempt: u32,
        pub input_manifest_hash: B256,
        pub tribute_count: u32,
        pub max_tributes_per_work_shard: u32,
        pub primary_work_unit_count: u32,
        pub primary_work_unit_root: B256,
        pub planner_spec_version: u16,
        pub reducer_spec_version: u16,
    }
}

wire_struct! {
    pub struct UnitArtifactV1 {
        pub protocol_bundle_hash: B256,
        pub job_id: B256,
        pub attempt: u32,
        pub unit_id: B256,
        pub phase: UnitPhase,
        pub interval_commitment: B256,
        pub input_root: B256,
        pub output_record_count: u32,
        pub canonical_output_bytes: BoundedBytes,
        pub output_semantic_digest: B256,
        pub coverage_or_permutation_commitment: B256,
    }
}
impl_top_level_codec!(UnitArtifactV1, UnitArtifactV1);

wire_struct! {
    pub struct RawTributeCoverageItemV1 {
        pub raw_ordinal: u32,
        pub tribute_id: EntityId36,
    }
}

wire_struct! {
    pub struct WorkOutputHeaderV1 {
        pub source_coverage_root: B256,
        pub output_coverage_root: B256,
        pub source_coverage_count: u32,
        pub output_coverage_count: u32,
    }
}

impl UnitSpecV1 {
    pub fn validate_semantics(&self, limits: &SchemaLimits) -> Result<(), ProtocolError> {
        require(
            self.canonical_ordered_inputs.len() <= limits.max_unit_inputs,
            "unit input cap",
        )?;
        let interval_matches = matches!(
            (&self.phase, &self.interval),
            (
                UnitPhase::Enumerate | UnitPhase::AmountMap | UnitPhase::OutputFinalize,
                UnitInterval::EntityIdRange(_)
            ) | (UnitPhase::FidelityMap, UnitInterval::FidelityIndexRange(_))
                | (
                    UnitPhase::FixedReduce
                        | UnitPhase::GratisPrefix
                        | UnitPhase::GratisPrefixDown
                        | UnitPhase::RootReduce,
                    UnitInterval::BinaryReducerNode(_)
                )
                | (
                    UnitPhase::OwnerShuffle | UnitPhase::BucketShuffle,
                    UnitInterval::CanonicalRunSpan(_)
                )
        );
        require(interval_matches, "phase interval binding")?;
        for input in &self.canonical_ordered_inputs {
            if input.source_kind == InputSourceKind::CanonicalEmpty {
                require(input.record_count_limit == 0, "canonical empty input count")?;
                require(
                    input.source_id == empty_unit_input_id(input.purpose)?,
                    "canonical empty input id",
                )?;
            }
        }
        Ok(())
    }

    pub fn unit_id(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        self.validate_semantics(limits)?;
        hash_framed(HashDomain::Unit, &self.encode_canonical(limits)?)
    }

    pub fn interval_commitment(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        let mut payload = vec![self.phase as u8];
        payload.extend_from_slice(&encode_nested_value(&self.interval, limits)?);
        hash_framed(HashDomain::UnitInterval, &payload)
    }

    pub fn input_root(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        let count = u32::try_from(self.canonical_ordered_inputs.len()).map_err(|_| {
            ProtocolError::IntegerOverflow {
                what: "unit input count",
            }
        })?;
        let mut payload = count.to_be_bytes().to_vec();
        for input in &self.canonical_ordered_inputs {
            payload.extend_from_slice(&encode_nested_value(input, limits)?);
        }
        hash_framed(HashDomain::UnitInputs, &payload)
    }
}

fn validate_unit_spec(spec: &UnitSpecV1, limits: &SchemaLimits) -> Result<(), ProtocolError> {
    spec.validate_semantics(limits)
}

impl UnitArtifactV1 {
    pub fn validate_against(
        &self,
        spec: &UnitSpecV1,
        limits: &SchemaLimits,
    ) -> Result<(), ProtocolError> {
        require(
            self.protocol_bundle_hash == spec.protocol_bundle_hash
                && self.job_id == spec.job_id
                && self.attempt == spec.attempt
                && self.phase == spec.phase,
            "unit artifact spec binding",
        )?;
        require(self.unit_id == spec.unit_id(limits)?, "unit artifact id")?;
        require(
            self.interval_commitment == spec.interval_commitment(limits)?,
            "unit artifact interval commitment",
        )?;
        require(
            self.input_root == spec.input_root(limits)?,
            "unit artifact input root",
        )?;
        let expected = unit_output_semantic_digest(self, limits)?;
        require(
            self.output_semantic_digest == expected,
            "unit output semantic digest",
        )
    }

    pub fn artifact_digest(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        hash_framed(HashDomain::UnitArtifact, &self.encode_canonical(limits)?)
    }
}

impl PlanCommitmentV1 {
    pub fn validate_semantics(&self) -> Result<(), ProtocolError> {
        require(
            self.tribute_count > 0
                && self.max_tributes_per_work_shard > 0
                && !self.primary_work_unit_root.is_zero(),
            "plan committed population",
        )?;
        let rounded = self
            .tribute_count
            .checked_add(self.max_tributes_per_work_shard - 1)
            .ok_or(ProtocolError::IntegerOverflow {
                what: "primary work unit count",
            })?;
        require(
            self.primary_work_unit_count == rounded / self.max_tributes_per_work_shard,
            "plan exact primary work unit count",
        )
    }

    pub fn plan_hash(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        self.validate_semantics()?;
        hash_framed(HashDomain::Plan, &encode_nested_value(self, limits)?)
    }
}

pub fn empty_unit_input_id(purpose: InputPurpose) -> Result<B256, ProtocolError> {
    hash_framed(HashDomain::UnitEmpty, &[purpose as u8])
}

pub fn unit_output_semantic_digest(
    artifact: &UnitArtifactV1,
    _limits: &SchemaLimits,
) -> Result<B256, ProtocolError> {
    let output_len = u32::try_from(artifact.canonical_output_bytes.0.len()).map_err(|_| {
        ProtocolError::IntegerOverflow {
            what: "unit output byte length",
        }
    })?;
    let mut payload = Vec::new();
    payload.extend_from_slice(artifact.protocol_bundle_hash.as_slice());
    payload.extend_from_slice(artifact.job_id.as_slice());
    payload.extend_from_slice(&artifact.attempt.to_be_bytes());
    payload.extend_from_slice(artifact.unit_id.as_slice());
    payload.push(artifact.phase as u8);
    payload.extend_from_slice(artifact.interval_commitment.as_slice());
    payload.extend_from_slice(&output_len.to_be_bytes());
    payload.extend_from_slice(&artifact.canonical_output_bytes.0);
    hash_framed(HashDomain::UnitOutput, &payload)
}
