use core::fmt;

use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    common::EntityId36,
    registry::ListKind,
    unit::{
        CanonicalInputRefV1, EntityIdHalfOpenRange, InputPurpose, InputSourceKind,
        PlanCommitmentV1, UnitInterval, UnitPhase, UnitSpecV1,
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
    MissingEntityId {
        ordinal: u32,
    },
    UnexpectedEntityId {
        ordinal: u32,
    },
    NonCanonicalEntityOrder {
        ordinal: u32,
    },
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
            Self::MissingEntityId { ordinal } => {
                write!(formatter, "canonical EntityId stream is missing ordinal {ordinal}")
            }
            Self::UnexpectedEntityId { ordinal } => {
                write!(formatter, "canonical EntityId stream has an extra ordinal {ordinal}")
            }
            Self::NonCanonicalEntityOrder { ordinal } => {
                write!(formatter, "canonical EntityId stream is not increasing at {ordinal}")
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
    pub tribute_collection_root: B256,
    pub tribute_input_encoded_bytes: u64,
    pub fidelity_opening_root: B256,
    pub oracle_opening_root: B256,
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

#[must_use = "the primary unit count is part of the plan commitment"]
pub fn primary_work_unit_count(tribute_count: u32) -> Result<u32, PlannerErrorV1> {
    if tribute_count == 0 {
        return Err(PlannerErrorV1::EmptyTributePopulation);
    }
    Ok(tribute_count / PRIMARY_WORK_SHARD_SIZE
        + u32::from(!tribute_count.is_multiple_of(PRIMARY_WORK_SHARD_SIZE)))
}

impl LysisPlannerV1 {
    pub fn new(bindings: LysisPlannerBindingsV1) -> Result<Self, PlannerErrorV1> {
        if bindings.tribute_count == 0 {
            return Err(PlannerErrorV1::EmptyTributePopulation);
        }
        if bindings.protocol_bundle_hash.is_zero()
            || bindings.job_id.is_zero()
            || bindings.input_manifest_hash.is_zero()
            || bindings.tribute_collection_root.is_zero()
            || bindings.fidelity_opening_root.is_zero()
            || bindings.oracle_opening_root.is_zero()
            || bindings.lysis_program_semantics_hash.is_zero()
            || bindings.input_manifest_encoded_bytes == 0
            || bindings.tribute_input_encoded_bytes == 0
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
        mut entity_id_at: F,
        limits: &SchemaLimits,
    ) -> Result<UnitSpecV1, PlannerErrorV1>
    where
        F: FnMut(u32) -> Option<EntityId36>,
    {
        let shard = self.primary_tree.primary_shard(shard_ordinal)?;
        let start = entity_id_at(shard.start_ordinal).ok_or(PlannerErrorV1::MissingEntityId {
            ordinal: shard.start_ordinal,
        })?;
        let end = if shard.end_ordinal < self.bindings.tribute_count {
            Some(
                entity_id_at(shard.end_ordinal).ok_or(PlannerErrorV1::MissingEntityId {
                    ordinal: shard.end_ordinal,
                })?,
            )
        } else {
            None
        };
        self.primary_unit_for_range(shard, start, end, limits)
    }

    pub fn commit_primary_catalog<I>(
        self,
        entity_ids: I,
        limits: &SchemaLimits,
    ) -> Result<PlanCommitmentV1, PlannerErrorV1>
    where
        I: IntoIterator<Item = EntityId36>,
    {
        let mut ids = entity_ids.into_iter();
        let mut previous = None;
        let mut shard_start = None;
        let mut shard = None;
        let mut root = StreamingOrderedListRoot::new(
            ListKind::UnitSpecificationsArtifacts,
            self.primary_work_unit_count(),
        )?;

        for ordinal in 0..self.bindings.tribute_count {
            let current = ids
                .next()
                .ok_or(PlannerErrorV1::MissingEntityId { ordinal })?;
            if previous.is_some_and(|prior| current <= prior) {
                return Err(PlannerErrorV1::NonCanonicalEntityOrder { ordinal });
            }
            if ordinal % PRIMARY_WORK_SHARD_SIZE == 0 {
                if let (Some(previous_shard), Some(start)) = (shard, shard_start) {
                    self.push_primary_spec(
                        &mut root,
                        previous_shard,
                        start,
                        Some(current),
                        limits,
                    )?;
                }
                let shard_ordinal = ordinal / PRIMARY_WORK_SHARD_SIZE;
                shard = Some(self.primary_tree.primary_shard(shard_ordinal)?);
                shard_start = Some(current);
            }
            previous = Some(current);
        }
        if ids.next().is_some() {
            return Err(PlannerErrorV1::UnexpectedEntityId {
                ordinal: self.bindings.tribute_count,
            });
        }
        self.push_primary_spec(
            &mut root,
            shard.ok_or(PlannerErrorV1::EmptyTributePopulation)?,
            shard_start.ok_or(PlannerErrorV1::EmptyTributePopulation)?,
            None,
            limits,
        )?;
        let primary_work_unit_root = root.finish()?;
        let plan = PlanCommitmentV1 {
            protocol_bundle_hash: self.bindings.protocol_bundle_hash,
            job_id: self.bindings.job_id,
            attempt: self.bindings.attempt,
            input_manifest_hash: self.bindings.input_manifest_hash,
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

    fn push_primary_spec(
        self,
        root: &mut StreamingOrderedListRoot,
        shard: PrimaryShardV1,
        start: EntityId36,
        end: Option<EntityId36>,
        limits: &SchemaLimits,
    ) -> Result<(), PlannerErrorV1> {
        let spec = self.primary_unit_for_range(shard, start, end, limits)?;
        root.push(&spec.encode_canonical(limits)?, limits.codec.max_body_bytes)?;
        Ok(())
    }

    fn primary_unit_for_range(
        self,
        shard: PrimaryShardV1,
        start: EntityId36,
        end: Option<EntityId36>,
        limits: &SchemaLimits,
    ) -> Result<UnitSpecV1, PlannerErrorV1> {
        if end.is_some_and(|end| start >= end) {
            return Err(PlannerErrorV1::NonCanonicalEntityOrder {
                ordinal: shard.end_ordinal,
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
                    source_id: self.bindings.tribute_collection_root,
                    record_count_limit: shard.record_count(),
                    max_encoded_bytes: self.bindings.tribute_input_encoded_bytes,
                    max_decoded_bytes: self.bindings.tribute_input_encoded_bytes,
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
