use outbe_ocomp_protocol::unit::{InputPurpose, UnitPhase};

use super::{LysisPlanTopologyV1, PlannedProducerV1, PlannedUnitPositionV1, PlannerErrorV1};

#[derive(Clone, Copy)]
struct BinaryChildShapeV1 {
    phase: UnitPhase,
    leaf_phase: UnitPhase,
    purpose: InputPurpose,
    full_tree: bool,
}

const FIXED_REDUCE_CHILDREN: BinaryChildShapeV1 = BinaryChildShapeV1 {
    phase: UnitPhase::FixedReduce,
    leaf_phase: UnitPhase::FidelityMap,
    purpose: InputPurpose::FidelityPartials,
    full_tree: true,
};

const GRATIS_PREFIX_CHILDREN: BinaryChildShapeV1 = BinaryChildShapeV1 {
    phase: UnitPhase::GratisPrefix,
    leaf_phase: UnitPhase::GratisPrefix,
    purpose: InputPurpose::GratisPrefixTable,
    full_tree: false,
};

const ROOT_REDUCE_CHILDREN: BinaryChildShapeV1 = BinaryChildShapeV1 {
    phase: UnitPhase::RootReduce,
    leaf_phase: UnitPhase::RootReduce,
    purpose: InputPurpose::RootSummary,
    full_tree: true,
};

impl LysisPlanTopologyV1 {
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
            } => self.binary_children(FIXED_REDUCE_CHILDREN, level, index),
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
            } => self.binary_children(GRATIS_PREFIX_CHILDREN, level, index),
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::GratisPrefixDown,
                level: 0,
                index,
            } if index < primary => Ok(vec![
                PlannedProducerV1::Unit(PlannedUnitPositionV1::TreeNode {
                    phase: UnitPhase::GratisPrefixDown,
                    level: 1,
                    index: index / 2,
                }),
                PlannedProducerV1::Unit(PlannedUnitPositionV1::TreeNode {
                    phase: UnitPhase::GratisPrefix,
                    level: 0,
                    index,
                }),
            ]),
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::GratisPrefixDown,
                level,
                index,
            } => {
                let child_summaries = self.binary_children(GRATIS_PREFIX_CHILDREN, level, index)?;
                if level == self.tree.height && index == 0 {
                    Ok(child_summaries)
                } else if level < self.tree.height {
                    let mut producers = Vec::with_capacity(3);
                    producers.push(PlannedProducerV1::Unit(PlannedUnitPositionV1::TreeNode {
                        phase: UnitPhase::GratisPrefixDown,
                        level: level + 1,
                        index: index / 2,
                    }));
                    producers.extend(child_summaries);
                    Ok(producers)
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
                self.run_children(phase, level, index)
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
            } => self.binary_children(ROOT_REDUCE_CHILDREN, level, index),
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

    fn shuffle_root(self, phase: UnitPhase) -> Result<PlannedUnitPositionV1, PlannerErrorV1> {
        self.run_span_root_position(phase, 0, self.tree.primary_leaf_count)
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
        shape: BinaryChildShapeV1,
        level: u16,
        index: u32,
    ) -> Result<Vec<PlannedProducerV1>, PlannerErrorV1> {
        if level == 0 || level > self.tree.height {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let width = if shape.full_tree {
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
            self.binary_child(shape, level, first),
            self.binary_child(shape, level, first + 1),
        ])
    }

    fn binary_child(
        self,
        shape: BinaryChildShapeV1,
        parent_level: u16,
        child_index: u32,
    ) -> PlannedProducerV1 {
        let child_level = parent_level - 1;
        let child_count = if child_level == 0 {
            self.tree.primary_leaf_count
        } else if shape.full_tree {
            self.tree.padded_leaf_count >> child_level
        } else {
            self.tree.primary_leaf_count.div_ceil(1_u32 << child_level)
        };
        if child_index >= child_count {
            return PlannedProducerV1::CanonicalEmpty {
                purpose: shape.purpose,
                padded_ordinal: child_index,
            };
        }
        let position = if child_level == 0 && shape.leaf_phase != shape.phase {
            PlannedUnitPositionV1::Primary {
                phase: shape.leaf_phase,
                ordinal: child_index,
            }
        } else {
            PlannedUnitPositionV1::TreeNode {
                phase: shape.phase,
                level: child_level,
                index: child_index,
            }
        };
        PlannedProducerV1::Unit(position)
    }

    fn run_children(
        self,
        phase: UnitPhase,
        level: u16,
        index: u32,
    ) -> Result<Vec<PlannedProducerV1>, PlannerErrorV1> {
        if level == 0 {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let position = self.run_span_position(phase, level, index)?;
        let PlannedUnitPositionV1::RunSpan {
            start_run, end_run, ..
        } = position
        else {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        };
        let width = end_run
            .checked_sub(start_run)
            .ok_or(PlannerErrorV1::IntegerOverflow)?;
        if width <= 1 {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let left_width = 1_u32 << (31 - (width - 1).leading_zeros());
        let split = start_run
            .checked_add(left_width)
            .ok_or(PlannerErrorV1::IntegerOverflow)?;
        Ok(vec![
            PlannedProducerV1::Unit(self.run_span_root_position(phase, start_run, split)?),
            PlannedProducerV1::Unit(self.run_span_root_position(phase, split, end_run)?),
        ])
    }

    fn run_span_root_position(
        self,
        phase: UnitPhase,
        start_run: u32,
        end_run: u32,
    ) -> Result<PlannedUnitPositionV1, PlannerErrorV1> {
        let width = end_run
            .checked_sub(start_run)
            .filter(|width| *width > 0)
            .ok_or(PlannerErrorV1::ProducerMembershipMismatch)?;
        let padded_width = width
            .checked_next_power_of_two()
            .ok_or(PlannerErrorV1::IntegerOverflow)?;
        if !start_run.is_multiple_of(padded_width) {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let level = u16::try_from(padded_width.trailing_zeros())
            .map_err(|_| PlannerErrorV1::IntegerOverflow)?;
        let position = self.run_span_position(phase, level, start_run / padded_width)?;
        match position {
            PlannedUnitPositionV1::RunSpan {
                start_run: actual_start,
                end_run: actual_end,
                ..
            } if actual_start == start_run && actual_end == end_run => Ok(position),
            _ => Err(PlannerErrorV1::ProducerMembershipMismatch),
        }
    }
}
