use core::fmt;

use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::{
    unit::{InputPurpose, UnitPhase},
    ProtocolError,
};

mod positions;
mod primary_units;
mod producers;
mod tree_units;

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
                write!(
                    formatter,
                    "reducer node ({level}, {index}) is outside the fixed tree"
                )
            }
            Self::MissingTributeChunk { ordinal } => {
                write!(formatter, "Tribute input chunk is missing shard {ordinal}")
            }
            Self::UnexpectedTributeChunk { ordinal } => {
                write!(
                    formatter,
                    "Tribute input chunk has an extra shard {ordinal}"
                )
            }
            Self::InvalidTributeChunk { ordinal } => {
                write!(
                    formatter,
                    "Tribute input chunk does not bind shard {ordinal}"
                )
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
            } => write!(
                formatter,
                "plan unit {ordinal} is outside 0..{total_unit_count}"
            ),
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
    pub lysis_limit_minor: U256,
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

impl PaddedBinaryTreeV1 {
    pub fn for_tribute_count(tribute_count: u32) -> Result<Self, PlannerErrorV1> {
        let primary_leaf_count = primary_work_unit_count(tribute_count)?;
        let mut tree = Self::for_primary_leaf_count(primary_leaf_count)?;
        tree.tribute_count = Some(tribute_count);
        Ok(tree)
    }

    pub fn for_primary_leaf_count(primary_leaf_count: u32) -> Result<Self, PlannerErrorV1> {
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
        let tribute_count = self.tribute_count.ok_or(PlannerErrorV1::IntegerOverflow)?;
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

    pub fn reducer_node(self, level: u16, index: u32) -> Result<ReducerNodeV1, PlannerErrorV1> {
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
