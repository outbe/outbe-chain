use core::fmt;

use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::{
    common::EntityId36,
    generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1,
    input::{InputChunkKind, InputChunkRefV1},
    registry::ListKind,
    unit::{
        CanonicalInputRefV1, EntityIdHalfOpenRange, FidelityIndexHalfOpenRange, InputPurpose,
        InputSourceKind, PlanCommitmentV1, UnitInterval, UnitPhase, UnitSpecV1,
    },
    ProtocolError, SchemaLimits, StreamingOrderedListRoot,
};

/// Frozen Lysis V1 source-shard width.
pub const PRIMARY_WORK_SHARD_SIZE: u32 = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlannerErrorV1 {
    EmptyTributePopulation,
    PrimaryShardOutOfRange {
        ordinal: u32,
        primary_leaf_count: u32,
    },
    ReducerNodeOutOfRange {
        level: u16,
        index: u32,
    },
    MissingTributeChunk {
        ordinal: u32,
    },
    UnexpectedTributeChunk {
        ordinal: u32,
    },
    InvalidTributeChunk {
        ordinal: u32,
    },
    PhasePositionOutOfRange {
        phase: UnitPhase,
        ordinal: u32,
        phase_unit_count: u32,
    },
    PlanPositionOutOfRange {
        ordinal: u32,
        total_unit_count: u32,
    },
    ProducerMembershipMismatch,
    IntegerOverflow,
    Protocol(ProtocolError),
}

impl fmt::Display for PlannerErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyTributePopulation => formatter.write_str("empty Tribute population"),
            Self::PrimaryShardOutOfRange {
                ordinal,
                primary_leaf_count,
            } => write!(
                formatter,
                "primary shard {ordinal} is outside 0..{primary_leaf_count}"
            ),
            Self::ReducerNodeOutOfRange { level, index } => {
                write!(formatter, "reducer node ({level}, {index}) is outside the fixed tree")
            }
            Self::MissingTributeChunk { ordinal } => {
                write!(formatter, "Tribute input chunk is missing shard {ordinal}")
            }
            Self::UnexpectedTributeChunk { ordinal } => {
                write!(formatter, "Tribute input chunk has an extra shard {ordinal}")
            }
            Self::InvalidTributeChunk { ordinal } => {
                write!(formatter, "Tribute input chunk does not bind shard {ordinal}")
            }
            Self::PhasePositionOutOfRange {
                phase,
                ordinal,
                phase_unit_count,
            } => write!(
                formatter,
                "{phase:?} unit {ordinal} is outside 0..{phase_unit_count}"
            ),
            Self::PlanPositionOutOfRange {
                ordinal,
                total_unit_count,
            } => write!(formatter, "plan unit {ordinal} is outside 0..{total_unit_count}"),
            Self::ProducerMembershipMismatch => {
                formatter.write_str("unit producer list is not the exact derived list")
            }
            Self::IntegerOverflow => formatter.write_str("planner integer overflow"),
            Self::Protocol(error) => write!(formatter, "planner protocol binding: {error}"),
        }
    }
}

impl std::error::Error for PlannerErrorV1 {}

impl From<ProtocolError> for PlannerErrorV1 {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrimaryShardV1 {
    pub ordinal: u32,
    pub start_ordinal: u32,
    pub end_ordinal: u32,
}

impl PrimaryShardV1 {
    #[must_use]
    pub const fn record_count(self) -> u32 {
        self.end_ordinal - self.start_ordinal
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReducerInputV1 {
    Primary(u32),
    CanonicalEmpty { padded_ordinal: u32 },
    Reducer { level: u16, index: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReducerNodeV1 {
    pub level: u16,
    pub index: u32,
    pub inputs: [ReducerInputV1; 2],
}

/// Constant-size description of the fixed padded binary topology.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaddedBinaryTreeV1 {
    tribute_count: Option<u32>,
    primary_leaf_count: u32,
    padded_leaf_count: u32,
    height: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LysisPlannerBindingsV1 {
    pub protocol_bundle_hash: B256,
    pub job_id: B256,
    pub attempt: u32,
    pub input_manifest_hash: B256,
    pub input_manifest_encoded_bytes: u64,
    pub fidelity_opening_root: B256,
    pub oracle_opening_root: B256,
    pub wwd: u32,
    pub lysis_budget: U256,
    pub logical_evaluation_time: u64,
    pub tribute_count: u32,
    pub lysis_program_semantics_hash: B256,
    pub planner_spec_version: u16,
    pub reducer_spec_version: u16,
}

/// Pure, constant-size Lysis V1 plan derivation state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LysisPlannerV1 {
    bindings: LysisPlannerBindingsV1,
    primary_tree: PaddedBinaryTreeV1,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PlannedUnitPositionV1 {
    Primary {
        phase: UnitPhase,
        ordinal: u32,
    },
    TreeNode {
        phase: UnitPhase,
        level: u16,
        index: u32,
    },
    RunSpan {
        phase: UnitPhase,
        level: u16,
        index: u32,
        start_run: u32,
        end_run: u32,
    },
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PlannedProducerV1 {
    Unit(PlannedUnitPositionV1),
    CanonicalEmpty {
        purpose: InputPurpose,
        padded_ordinal: u32,
    },
}

impl PlannedUnitPositionV1 {
    #[must_use]
    pub const fn phase(&self) -> UnitPhase {
        match self {
            Self::Primary { phase, .. }
            | Self::TreeNode { phase, .. }
            | Self::RunSpan { phase, .. } => *phase,
        }
    }
}

/// Closed Lysis V1 phase topology. It has no runtime registration surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LysisPlanTopologyV1 {
    tree: PaddedBinaryTreeV1,
}

const LYSIS_PLAN_PHASE_ORDER: [UnitPhase; 10] = [
    UnitPhase::Enumerate,
    UnitPhase::FidelityMap,
    UnitPhase::FixedReduce,
    UnitPhase::AmountMap,
    UnitPhase::GratisPrefix,
    UnitPhase::GratisPrefixDown,
    UnitPhase::OutputFinalize,
    UnitPhase::OwnerShuffle,
    UnitPhase::BucketShuffle,
    UnitPhase::RootReduce,
];

#[must_use = "the primary unit count is part of the plan commitment"]
pub fn primary_work_unit_count(tribute_count: u32) -> Result<u32, PlannerErrorV1> {
    if tribute_count == 0 {
        return Err(PlannerErrorV1::EmptyTributePopulation);
    }
    Ok(tribute_count / PRIMARY_WORK_SHARD_SIZE
        + u32::from(!tribute_count.is_multiple_of(PRIMARY_WORK_SHARD_SIZE)))
}

fn chunk_first_id(
    chunk: &InputChunkRefV1,
    shard_ordinal: u32,
) -> Result<EntityId36, PlannerErrorV1> {
    let bytes: [u8; 36] = chunk
        .first_key
        .0
        .as_slice()
        .try_into()
        .map_err(|_| PlannerErrorV1::InvalidTributeChunk {
            ordinal: shard_ordinal,
        })?;
    Ok(EntityId36(bytes))
}

impl LysisPlannerV1 {
    pub fn new(bindings: LysisPlannerBindingsV1) -> Result<Self, PlannerErrorV1> {
        if bindings.tribute_count == 0 {
            return Err(PlannerErrorV1::EmptyTributePopulation);
        }
        if bindings.protocol_bundle_hash.is_zero()
            || bindings.job_id.is_zero()
            || bindings.input_manifest_hash.is_zero()
            || bindings.fidelity_opening_root.is_zero()
            || bindings.oracle_opening_root.is_zero()
            || bindings.wwd == 0
            || bindings.lysis_budget.is_zero()
            || bindings.logical_evaluation_time == 0
            || bindings.lysis_program_semantics_hash.is_zero()
            || bindings.input_manifest_encoded_bytes == 0
            || bindings.planner_spec_version == 0
            || bindings.reducer_spec_version == 0
        {
            return Err(ProtocolError::InvalidInvariant("Lysis planner frozen bindings").into());
        }
        Ok(Self {
            primary_tree: PaddedBinaryTreeV1::for_tribute_count(bindings.tribute_count)?,
            bindings,
        })
    }

    #[must_use]
    pub const fn primary_work_unit_count(self) -> u32 {
        self.primary_tree.primary_leaf_count
    }

    pub fn primary_unit_at<F>(
        self,
        shard_ordinal: u32,
        mut tribute_chunk_at: F,
        limits: &SchemaLimits,
    ) -> Result<UnitSpecV1, PlannerErrorV1>
    where
        F: FnMut(u32) -> Option<InputChunkRefV1>,
    {
        let shard = self.primary_tree.primary_shard(shard_ordinal)?;
        let tribute_chunk =
            tribute_chunk_at(shard_ordinal).ok_or(PlannerErrorV1::MissingTributeChunk {
                ordinal: shard_ordinal,
            })?;
        let start = chunk_first_id(&tribute_chunk, shard_ordinal)?;
        let end = if shard.end_ordinal < self.bindings.tribute_count {
            let next_ordinal = shard_ordinal
                .checked_add(1)
                .ok_or(PlannerErrorV1::IntegerOverflow)?;
            Some(chunk_first_id(
                &tribute_chunk_at(next_ordinal).ok_or(
                    PlannerErrorV1::MissingTributeChunk {
                        ordinal: next_ordinal,
                    },
                )?,
                next_ordinal,
            )?)
        } else {
            None
        };
        self.primary_unit_for_range(shard, start, end, &tribute_chunk, limits)
    }

    pub fn commit_primary_catalog<I>(
        self,
        tribute_chunks: I,
        limits: &SchemaLimits,
    ) -> Result<PlanCommitmentV1, PlannerErrorV1>
    where
        I: IntoIterator<Item = InputChunkRefV1>,
    {
        let mut chunks = tribute_chunks.into_iter().peekable();
        let mut root = StreamingOrderedListRoot::new(
            ListKind::UnitSpecificationsArtifacts,
            self.primary_work_unit_count(),
        )?;

        for shard_ordinal in 0..self.primary_work_unit_count() {
            let tribute_chunk = chunks
                .next()
                .ok_or(PlannerErrorV1::MissingTributeChunk {
                    ordinal: shard_ordinal,
                })?;
            let shard = self.primary_tree.primary_shard(shard_ordinal)?;
            let start = chunk_first_id(&tribute_chunk, shard_ordinal)?;
            let end = chunks
                .peek()
                .map(|next| chunk_first_id(next, shard_ordinal + 1))
                .transpose()?;
            self.push_primary_spec(&mut root, shard, start, end, &tribute_chunk, limits)?;
        }
        if chunks.next().is_some() {
            return Err(PlannerErrorV1::UnexpectedTributeChunk {
                ordinal: self.primary_work_unit_count(),
            });
        }
        let primary_work_unit_root = root.finish()?;
        let plan = PlanCommitmentV1 {
            protocol_bundle_hash: self.bindings.protocol_bundle_hash,
            job_id: self.bindings.job_id,
            attempt: self.bindings.attempt,
            input_manifest_hash: self.bindings.input_manifest_hash,
            wwd: self.bindings.wwd,
            lysis_budget: self.bindings.lysis_budget,
            logical_evaluation_time: self.bindings.logical_evaluation_time,
            tribute_count: self.bindings.tribute_count,
            max_tributes_per_work_shard: PRIMARY_WORK_SHARD_SIZE,
            primary_work_unit_count: self.primary_work_unit_count(),
            primary_work_unit_root,
            planner_spec_version: self.bindings.planner_spec_version,
            reducer_spec_version: self.bindings.reducer_spec_version,
        };
        plan.validate_semantics()?;
        Ok(plan)
    }

    pub fn fidelity_map_unit_at(
        self,
        shard_ordinal: u32,
        enumerate_unit_id: B256,
        limits: &SchemaLimits,
    ) -> Result<UnitSpecV1, PlannerErrorV1> {
        if enumerate_unit_id.is_zero() {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let shard = self.primary_tree.primary_shard(shard_ordinal)?;
        let candidate = OCOMP_POC_CANDIDATE_LIMITS_V1;
        let max_fidelity_opening_bytes = candidate
            .max_fidelity_openings_per_work_shard
            .checked_mul(candidate.max_opening_bytes)
            .ok_or(PlannerErrorV1::IntegerOverflow)?;
        let fidelity_opening_count_limit =
            u32::try_from(candidate.max_fidelity_openings_per_work_shard)
                .map_err(|_| PlannerErrorV1::IntegerOverflow)?;
        let spec = UnitSpecV1 {
            protocol_bundle_hash: self.bindings.protocol_bundle_hash,
            job_id: self.bindings.job_id,
            attempt: self.bindings.attempt,
            phase: UnitPhase::FidelityMap,
            interval: UnitInterval::FidelityIndexRange(FidelityIndexHalfOpenRange {
                start: shard.start_ordinal,
                end: shard.end_ordinal,
            }),
            canonical_ordered_inputs: vec![
                CanonicalInputRefV1 {
                    purpose: InputPurpose::InputManifest,
                    source_kind: InputSourceKind::AuthenticatedRoot,
                    source_id: self.bindings.input_manifest_hash,
                    record_count_limit: 1,
                    max_encoded_bytes: self.bindings.input_manifest_encoded_bytes,
                    max_decoded_bytes: self.bindings.input_manifest_encoded_bytes,
                },
                CanonicalInputRefV1 {
                    purpose: InputPurpose::EnumeratedTributes,
                    source_kind: InputSourceKind::UnitOutput,
                    source_id: enumerate_unit_id,
                    record_count_limit: shard.record_count(),
                    max_encoded_bytes: candidate.max_activation_ocb1_bytes,
                    max_decoded_bytes: candidate.max_activation_ocb1_bytes,
                },
                CanonicalInputRefV1 {
                    purpose: InputPurpose::FidelityOpenings,
                    source_kind: InputSourceKind::AuthenticatedRoot,
                    source_id: self.bindings.fidelity_opening_root,
                    record_count_limit: fidelity_opening_count_limit,
                    max_encoded_bytes: max_fidelity_opening_bytes,
                    max_decoded_bytes: max_fidelity_opening_bytes,
                },
            ],
            lysis_program_semantics_hash: self.bindings.lysis_program_semantics_hash,
            planner_spec_version: self.bindings.planner_spec_version,
            reducer_spec_version: self.bindings.reducer_spec_version,
        };
        spec.validate_semantics(limits)?;
        Ok(spec)
    }

    fn push_primary_spec(
        self,
        root: &mut StreamingOrderedListRoot,
        shard: PrimaryShardV1,
        start: EntityId36,
        end: Option<EntityId36>,
        tribute_chunk: &InputChunkRefV1,
        limits: &SchemaLimits,
    ) -> Result<(), PlannerErrorV1> {
        let spec = self.primary_unit_for_range(shard, start, end, tribute_chunk, limits)?;
        root.push(&spec.encode_canonical(limits)?, limits.codec.max_body_bytes)?;
        Ok(())
    }

    fn primary_unit_for_range(
        self,
        shard: PrimaryShardV1,
        start: EntityId36,
        end: Option<EntityId36>,
        tribute_chunk: &InputChunkRefV1,
        limits: &SchemaLimits,
    ) -> Result<UnitSpecV1, PlannerErrorV1> {
        tribute_chunk.encode_canonical_record(limits)?;
        if tribute_chunk.kind != InputChunkKind::Tribute
            || tribute_chunk.ordinal != shard.ordinal
            || tribute_chunk.record_count != shard.record_count()
            || tribute_chunk.first_key.0.as_slice() != start.0
            || tribute_chunk.last_key_inclusive.0.len() != 36
            || tribute_chunk.last_key_inclusive.0.as_slice() < start.0.as_slice()
            || end.is_some_and(|end| {
                tribute_chunk.last_key_inclusive.0.as_slice() >= end.0.as_slice()
            })
        {
            return Err(PlannerErrorV1::InvalidTributeChunk {
                ordinal: shard.ordinal,
            });
        }
        let spec = UnitSpecV1 {
            protocol_bundle_hash: self.bindings.protocol_bundle_hash,
            job_id: self.bindings.job_id,
            attempt: self.bindings.attempt,
            phase: UnitPhase::Enumerate,
            interval: UnitInterval::EntityIdRange(EntityIdHalfOpenRange { start, end }),
            canonical_ordered_inputs: vec![
                CanonicalInputRefV1 {
                    purpose: InputPurpose::InputManifest,
                    source_kind: InputSourceKind::AuthenticatedRoot,
                    source_id: self.bindings.input_manifest_hash,
                    record_count_limit: 1,
                    max_encoded_bytes: self.bindings.input_manifest_encoded_bytes,
                    max_decoded_bytes: self.bindings.input_manifest_encoded_bytes,
                },
                CanonicalInputRefV1 {
                    purpose: InputPurpose::TributeStream,
                    source_kind: InputSourceKind::AuthenticatedRoot,
                    source_id: tribute_chunk.semantic_digest,
                    record_count_limit: shard.record_count(),
                    max_encoded_bytes: tribute_chunk.encoded_bytes,
                    max_decoded_bytes: tribute_chunk.encoded_bytes,
                },
            ],
            lysis_program_semantics_hash: self.bindings.lysis_program_semantics_hash,
            planner_spec_version: self.bindings.planner_spec_version,
            reducer_spec_version: self.bindings.reducer_spec_version,
        };
        spec.validate_semantics(limits)?;
        Ok(spec)
    }
}

impl LysisPlanTopologyV1 {
    pub fn new(primary_leaf_count: u32) -> Result<Self, PlannerErrorV1> {
        Ok(Self {
            tree: PaddedBinaryTreeV1::for_primary_leaf_count(primary_leaf_count)?,
        })
    }

    #[must_use]
    pub const fn tree(self) -> PaddedBinaryTreeV1 {
        self.tree
    }

    #[must_use]
    pub fn phase_unit_count(self, phase: UnitPhase) -> u32 {
        let primary = self.tree.primary_leaf_count;
        let full_internal = self.tree.reducer_node_count();
        let active_internal = self.active_internal_node_count();
        match phase {
            UnitPhase::Enumerate
            | UnitPhase::FidelityMap
            | UnitPhase::AmountMap
            | UnitPhase::OutputFinalize => primary,
            UnitPhase::FixedReduce => full_internal,
            UnitPhase::GratisPrefix
            | UnitPhase::GratisPrefixDown
            | UnitPhase::OwnerShuffle
            | UnitPhase::BucketShuffle => primary + active_internal,
            UnitPhase::RootReduce => primary + full_internal,
        }
    }

    #[must_use]
    pub fn total_unit_count(self) -> u32 {
        LYSIS_PLAN_PHASE_ORDER
            .into_iter()
            .map(|phase| self.phase_unit_count(phase))
            .sum()
    }

    pub fn plan_position_at(
        self,
        ordinal: u32,
    ) -> Result<PlannedUnitPositionV1, PlannerErrorV1> {
        let mut offset = 0_u32;
        for phase in LYSIS_PLAN_PHASE_ORDER {
            let count = self.phase_unit_count(phase);
            if ordinal < offset + count {
                return self.phase_position_at(phase, ordinal - offset);
            }
            offset += count;
        }
        Err(PlannerErrorV1::PlanPositionOutOfRange {
            ordinal,
            total_unit_count: offset,
        })
    }

    pub fn phase_position_at(
        self,
        phase: UnitPhase,
        ordinal: u32,
    ) -> Result<PlannedUnitPositionV1, PlannerErrorV1> {
        let count = self.phase_unit_count(phase);
        if ordinal >= count {
            return Err(PlannerErrorV1::PhasePositionOutOfRange {
                phase,
                ordinal,
                phase_unit_count: count,
            });
        }
        let primary = self.tree.primary_leaf_count;
        match phase {
            UnitPhase::Enumerate
            | UnitPhase::FidelityMap
            | UnitPhase::AmountMap
            | UnitPhase::OutputFinalize => Ok(PlannedUnitPositionV1::Primary {
                phase,
                ordinal,
            }),
            UnitPhase::FixedReduce => {
                let (level, index) = self.full_bottom_up_node_at(ordinal)?;
                Ok(PlannedUnitPositionV1::TreeNode {
                    phase,
                    level,
                    index,
                })
            }
            UnitPhase::GratisPrefix => {
                if ordinal < primary {
                    Ok(PlannedUnitPositionV1::TreeNode {
                        phase,
                        level: 0,
                        index: ordinal,
                    })
                } else {
                    let (level, index) = self.active_bottom_up_node_at(ordinal - primary)?;
                    Ok(PlannedUnitPositionV1::TreeNode {
                        phase,
                        level,
                        index,
                    })
                }
            }
            UnitPhase::GratisPrefixDown => {
                let active_internal = self.active_internal_node_count();
                if ordinal < active_internal {
                    let (level, index) = self.active_top_down_node_at(ordinal)?;
                    Ok(PlannedUnitPositionV1::TreeNode {
                        phase,
                        level,
                        index,
                    })
                } else {
                    Ok(PlannedUnitPositionV1::TreeNode {
                        phase,
                        level: 0,
                        index: ordinal - active_internal,
                    })
                }
            }
            UnitPhase::OwnerShuffle | UnitPhase::BucketShuffle => {
                let (level, index) = if ordinal < primary {
                    (0, ordinal)
                } else {
                    self.active_bottom_up_node_at(ordinal - primary)?
                };
                let width = 1_u32
                    .checked_shl(u32::from(level))
                    .ok_or(PlannerErrorV1::IntegerOverflow)?;
                let start_run = index
                    .checked_mul(width)
                    .ok_or(PlannerErrorV1::IntegerOverflow)?;
                let end_run = start_run.saturating_add(width).min(primary);
                Ok(PlannedUnitPositionV1::RunSpan {
                    phase,
                    level,
                    index,
                    start_run,
                    end_run,
                })
            }
            UnitPhase::RootReduce => {
                if ordinal < primary {
                    Ok(PlannedUnitPositionV1::TreeNode {
                        phase,
                        level: 0,
                        index: ordinal,
                    })
                } else {
                    let (level, index) = self.full_bottom_up_node_at(ordinal - primary)?;
                    Ok(PlannedUnitPositionV1::TreeNode {
                        phase,
                        level,
                        index,
                    })
                }
            }
        }
    }

    pub fn required_producers(
        self,
        consumer: PlannedUnitPositionV1,
    ) -> Result<Vec<PlannedProducerV1>, PlannerErrorV1> {
        let primary = self.tree.primary_leaf_count;
        match consumer {
            PlannedUnitPositionV1::Primary {
                phase: UnitPhase::Enumerate,
                ..
            } => Ok(Vec::new()),
            PlannedUnitPositionV1::Primary {
                phase: UnitPhase::FidelityMap,
                ordinal,
            } if ordinal < primary => Ok(vec![PlannedProducerV1::Unit(
                PlannedUnitPositionV1::Primary {
                    phase: UnitPhase::Enumerate,
                    ordinal,
                },
            )]),
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::FixedReduce,
                level,
                index,
            } => self.binary_children(
                UnitPhase::FixedReduce,
                UnitPhase::FidelityMap,
                InputPurpose::FidelityPartials,
                level,
                index,
                true,
            ),
            PlannedUnitPositionV1::Primary {
                phase: UnitPhase::AmountMap,
                ordinal,
            } if ordinal < primary => Ok(vec![
                PlannedProducerV1::Unit(PlannedUnitPositionV1::Primary {
                    phase: UnitPhase::Enumerate,
                    ordinal,
                }),
                PlannedProducerV1::Unit(PlannedUnitPositionV1::Primary {
                    phase: UnitPhase::FidelityMap,
                    ordinal,
                }),
                PlannedProducerV1::Unit(self.fixed_reduce_root()),
            ]),
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::GratisPrefix,
                level: 0,
                index,
            } if index < primary => Ok(vec![PlannedProducerV1::Unit(
                PlannedUnitPositionV1::Primary {
                    phase: UnitPhase::AmountMap,
                    ordinal: index,
                },
            )]),
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::GratisPrefix,
                level,
                index,
            } => self.binary_children(
                UnitPhase::GratisPrefix,
                UnitPhase::GratisPrefix,
                InputPurpose::GratisPrefixTable,
                level,
                index,
                false,
            ),
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::GratisPrefixDown,
                level,
                index,
            } => {
                let prefix = PlannedProducerV1::Unit(PlannedUnitPositionV1::TreeNode {
                    phase: UnitPhase::GratisPrefix,
                    level,
                    index,
                });
                if level == self.tree.height && index == 0 {
                    Ok(vec![prefix])
                } else if level < self.tree.height {
                    Ok(vec![
                        PlannedProducerV1::Unit(PlannedUnitPositionV1::TreeNode {
                            phase: UnitPhase::GratisPrefixDown,
                            level: level + 1,
                            index: index / 2,
                        }),
                        prefix,
                    ])
                } else {
                    Err(PlannerErrorV1::ProducerMembershipMismatch)
                }
            }
            PlannedUnitPositionV1::Primary {
                phase: UnitPhase::OutputFinalize,
                ordinal,
            } if ordinal < primary => Ok(vec![
                PlannedProducerV1::Unit(PlannedUnitPositionV1::Primary {
                    phase: UnitPhase::AmountMap,
                    ordinal,
                }),
                PlannedProducerV1::Unit(PlannedUnitPositionV1::TreeNode {
                    phase: UnitPhase::GratisPrefixDown,
                    level: 0,
                    index: ordinal,
                }),
            ]),
            PlannedUnitPositionV1::RunSpan {
                phase: UnitPhase::OwnerShuffle | UnitPhase::BucketShuffle,
                level: 0,
                index,
                start_run,
                end_run,
            } if index < primary && start_run == index && end_run == index + 1 => {
                Ok(vec![PlannedProducerV1::Unit(
                    PlannedUnitPositionV1::Primary {
                        phase: UnitPhase::OutputFinalize,
                        ordinal: index,
                    },
                )])
            }
            PlannedUnitPositionV1::RunSpan {
                phase,
                level,
                index,
                start_run,
                end_run,
            } if matches!(phase, UnitPhase::OwnerShuffle | UnitPhase::BucketShuffle)
                && level > 0 =>
            {
                let expected = self.run_span_position(phase, level, index)?;
                if expected
                    != (PlannedUnitPositionV1::RunSpan {
                        phase,
                        level,
                        index,
                        start_run,
                        end_run,
                    })
                {
                    return Err(PlannerErrorV1::ProducerMembershipMismatch);
                }
                let purpose = if phase == UnitPhase::OwnerShuffle {
                    InputPurpose::OwnerOrderedRecords
                } else {
                    InputPurpose::BucketOrderedRecords
                };
                self.run_children(phase, purpose, level, index)
            }
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::RootReduce,
                level: 0,
                index,
            } if index < primary => Ok(vec![
                PlannedProducerV1::Unit(PlannedUnitPositionV1::Primary {
                    phase: UnitPhase::OutputFinalize,
                    ordinal: index,
                }),
                PlannedProducerV1::Unit(self.shuffle_root(UnitPhase::OwnerShuffle)?),
                PlannedProducerV1::Unit(self.shuffle_root(UnitPhase::BucketShuffle)?),
            ]),
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::RootReduce,
                level,
                index,
            } => self.binary_children(
                UnitPhase::RootReduce,
                UnitPhase::RootReduce,
                InputPurpose::RootSummary,
                level,
                index,
                true,
            ),
            _ => Err(PlannerErrorV1::ProducerMembershipMismatch),
        }
    }

    pub fn validate_exact_producers(
        self,
        consumer: PlannedUnitPositionV1,
        actual: &[PlannedProducerV1],
    ) -> Result<(), PlannerErrorV1> {
        if self.required_producers(consumer)? == actual {
            Ok(())
        } else {
            Err(PlannerErrorV1::ProducerMembershipMismatch)
        }
    }

    fn fixed_reduce_root(self) -> PlannedUnitPositionV1 {
        PlannedUnitPositionV1::TreeNode {
            phase: UnitPhase::FixedReduce,
            level: self.tree.height,
            index: 0,
        }
    }

    fn shuffle_root(
        self,
        phase: UnitPhase,
    ) -> Result<PlannedUnitPositionV1, PlannerErrorV1> {
        self.run_span_position(phase, self.tree.height, 0)
    }

    fn run_span_position(
        self,
        phase: UnitPhase,
        level: u16,
        index: u32,
    ) -> Result<PlannedUnitPositionV1, PlannerErrorV1> {
        let width = 1_u32
            .checked_shl(u32::from(level))
            .ok_or(PlannerErrorV1::IntegerOverflow)?;
        let start_run = index
            .checked_mul(width)
            .ok_or(PlannerErrorV1::IntegerOverflow)?;
        let end_run = start_run
            .saturating_add(width)
            .min(self.tree.primary_leaf_count);
        if start_run >= end_run {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        Ok(PlannedUnitPositionV1::RunSpan {
            phase,
            level,
            index,
            start_run,
            end_run,
        })
    }

    fn binary_children(
        self,
        phase: UnitPhase,
        leaf_phase: UnitPhase,
        purpose: InputPurpose,
        level: u16,
        index: u32,
        full_tree: bool,
    ) -> Result<Vec<PlannedProducerV1>, PlannerErrorV1> {
        if level == 0 || level > self.tree.height {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let width = if full_tree {
            self.tree.padded_leaf_count >> level
        } else {
            self.tree.primary_leaf_count.div_ceil(1_u32 << level)
        };
        if index >= width {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let first = index
            .checked_mul(2)
            .ok_or(PlannerErrorV1::IntegerOverflow)?;
        Ok(vec![
            self.binary_child(phase, leaf_phase, purpose, level, first, full_tree),
            self.binary_child(phase, leaf_phase, purpose, level, first + 1, full_tree),
        ])
    }

    fn binary_child(
        self,
        phase: UnitPhase,
        leaf_phase: UnitPhase,
        purpose: InputPurpose,
        parent_level: u16,
        child_index: u32,
        full_tree: bool,
    ) -> PlannedProducerV1 {
        let child_level = parent_level - 1;
        let child_count = if child_level == 0 {
            self.tree.primary_leaf_count
        } else if full_tree {
            self.tree.padded_leaf_count >> child_level
        } else {
            self.tree
                .primary_leaf_count
                .div_ceil(1_u32 << child_level)
        };
        if child_index >= child_count {
            return PlannedProducerV1::CanonicalEmpty {
                purpose,
                padded_ordinal: child_index,
            };
        }
        let position = if child_level == 0 && leaf_phase != phase {
            PlannedUnitPositionV1::Primary {
                phase: leaf_phase,
                ordinal: child_index,
            }
        } else {
            PlannedUnitPositionV1::TreeNode {
                phase,
                level: child_level,
                index: child_index,
            }
        };
        PlannedProducerV1::Unit(position)
    }

    fn run_children(
        self,
        phase: UnitPhase,
        purpose: InputPurpose,
        level: u16,
        index: u32,
    ) -> Result<Vec<PlannedProducerV1>, PlannerErrorV1> {
        if level == 0 {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let first = index
            .checked_mul(2)
            .ok_or(PlannerErrorV1::IntegerOverflow)?;
        let child_level = level - 1;
        let child_count = if child_level == 0 {
            self.tree.primary_leaf_count
        } else {
            self.tree
                .primary_leaf_count
                .div_ceil(1_u32 << child_level)
        };
        let mut inputs = Vec::with_capacity(2);
        for child_index in [first, first + 1] {
            if child_index < child_count {
                inputs.push(PlannedProducerV1::Unit(self.run_span_position(
                    phase,
                    child_level,
                    child_index,
                )?));
            } else {
                inputs.push(PlannedProducerV1::CanonicalEmpty {
                    purpose,
                    padded_ordinal: child_index,
                });
            }
        }
        Ok(inputs)
    }

    fn active_internal_node_count(self) -> u32 {
        (1..=self.tree.height)
            .map(|level| self.tree.primary_leaf_count.div_ceil(1_u32 << level))
            .sum()
    }

    fn full_bottom_up_node_at(self, mut ordinal: u32) -> Result<(u16, u32), PlannerErrorV1> {
        for level in 1..=self.tree.height {
            let width = self.tree.padded_leaf_count >> level;
            if ordinal < width {
                return Ok((level, ordinal));
            }
            ordinal -= width;
        }
        Err(PlannerErrorV1::IntegerOverflow)
    }

    fn active_bottom_up_node_at(
        self,
        mut ordinal: u32,
    ) -> Result<(u16, u32), PlannerErrorV1> {
        for level in 1..=self.tree.height {
            let width = self.tree.primary_leaf_count.div_ceil(1_u32 << level);
            if ordinal < width {
                return Ok((level, ordinal));
            }
            ordinal -= width;
        }
        Err(PlannerErrorV1::IntegerOverflow)
    }

    fn active_top_down_node_at(
        self,
        mut ordinal: u32,
    ) -> Result<(u16, u32), PlannerErrorV1> {
        for level in (1..=self.tree.height).rev() {
            let width = self.tree.primary_leaf_count.div_ceil(1_u32 << level);
            if ordinal < width {
                return Ok((level, ordinal));
            }
            ordinal -= width;
        }
        Err(PlannerErrorV1::IntegerOverflow)
    }
}

impl PaddedBinaryTreeV1 {
    pub fn for_tribute_count(tribute_count: u32) -> Result<Self, PlannerErrorV1> {
        let primary_leaf_count = primary_work_unit_count(tribute_count)?;
        let mut tree = Self::for_primary_leaf_count(primary_leaf_count)?;
        tree.tribute_count = Some(tribute_count);
        Ok(tree)
    }

    pub fn for_primary_leaf_count(
        primary_leaf_count: u32,
    ) -> Result<Self, PlannerErrorV1> {
        if primary_leaf_count == 0 {
            return Err(PlannerErrorV1::EmptyTributePopulation);
        }
        let padded_leaf_count = primary_leaf_count
            .checked_next_power_of_two()
            .ok_or(PlannerErrorV1::IntegerOverflow)?
            .max(2);
        let height = u16::try_from(padded_leaf_count.trailing_zeros())
            .map_err(|_| PlannerErrorV1::IntegerOverflow)?;
        Ok(Self {
            tribute_count: None,
            primary_leaf_count,
            padded_leaf_count,
            height,
        })
    }

    #[must_use]
    pub const fn primary_leaf_count(self) -> u32 {
        self.primary_leaf_count
    }

    #[must_use]
    pub const fn padded_leaf_count(self) -> u32 {
        self.padded_leaf_count
    }

    #[must_use]
    pub const fn height(self) -> u16 {
        self.height
    }

    #[must_use]
    pub const fn reducer_node_count(self) -> u32 {
        self.padded_leaf_count - 1
    }

    pub fn primary_shard(self, ordinal: u32) -> Result<PrimaryShardV1, PlannerErrorV1> {
        if ordinal >= self.primary_leaf_count {
            return Err(PlannerErrorV1::PrimaryShardOutOfRange {
                ordinal,
                primary_leaf_count: self.primary_leaf_count,
            });
        }
        let tribute_count = self
            .tribute_count
            .ok_or(PlannerErrorV1::IntegerOverflow)?;
        let start_ordinal = ordinal
            .checked_mul(PRIMARY_WORK_SHARD_SIZE)
            .ok_or(PlannerErrorV1::IntegerOverflow)?;
        let end_ordinal = start_ordinal
            .saturating_add(PRIMARY_WORK_SHARD_SIZE)
            .min(tribute_count);
        Ok(PrimaryShardV1 {
            ordinal,
            start_ordinal,
            end_ordinal,
        })
    }

    pub fn reducer_node(
        self,
        level: u16,
        index: u32,
    ) -> Result<ReducerNodeV1, PlannerErrorV1> {
        if level == 0 || level > self.height {
            return Err(PlannerErrorV1::ReducerNodeOutOfRange { level, index });
        }
        let width = self.padded_leaf_count >> level;
        if index >= width {
            return Err(PlannerErrorV1::ReducerNodeOutOfRange { level, index });
        }
        let child_index = index
            .checked_mul(2)
            .ok_or(PlannerErrorV1::IntegerOverflow)?;
        let inputs = if level == 1 {
            [
                self.leaf_input(child_index),
                self.leaf_input(child_index + 1),
            ]
        } else {
            [
                ReducerInputV1::Reducer {
                    level: level - 1,
                    index: child_index,
                },
                ReducerInputV1::Reducer {
                    level: level - 1,
                    index: child_index + 1,
                },
            ]
        };
        Ok(ReducerNodeV1 {
            level,
            index,
            inputs,
        })
    }

    const fn leaf_input(self, ordinal: u32) -> ReducerInputV1 {
        if ordinal < self.primary_leaf_count {
            ReducerInputV1::Primary(ordinal)
        } else {
            ReducerInputV1::CanonicalEmpty {
                padded_ordinal: ordinal,
            }
        }
    }
}
