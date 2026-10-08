use outbe_ocomp_protocol::unit::UnitPhase;

use super::{
    LysisPlanTopologyV1, PaddedBinaryTreeV1, PlannedUnitPositionV1, PlannerErrorV1,
    LYSIS_PLAN_PHASE_ORDER,
};

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
            UnitPhase::GratisPrefix | UnitPhase::GratisPrefixDown => primary + active_internal,
            UnitPhase::OwnerShuffle | UnitPhase::BucketShuffle => primary + (primary - 1),
            UnitPhase::RootReduce if primary == 1 => 1,
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

    pub fn plan_position_at(self, ordinal: u32) -> Result<PlannedUnitPositionV1, PlannerErrorV1> {
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

    pub fn plan_ordinal_of(self, position: PlannedUnitPositionV1) -> Result<u32, PlannerErrorV1> {
        let phase = position.phase();
        let phase_ordinal = self.phase_ordinal_of(position)?;
        self.phase_offset(phase)?
            .checked_add(phase_ordinal)
            .ok_or(PlannerErrorV1::IntegerOverflow)
    }

    pub fn phase_offset(self, target: UnitPhase) -> Result<u32, PlannerErrorV1> {
        let mut offset = 0_u32;
        for phase in LYSIS_PLAN_PHASE_ORDER {
            if phase == target {
                return Ok(offset);
            }
            offset = offset
                .checked_add(self.phase_unit_count(phase))
                .ok_or(PlannerErrorV1::IntegerOverflow)?;
        }
        Err(PlannerErrorV1::ProducerMembershipMismatch)
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
        let active_internal = self.active_internal_node_count();
        let (level, index) = match phase {
            UnitPhase::Enumerate
            | UnitPhase::FidelityMap
            | UnitPhase::AmountMap
            | UnitPhase::OutputFinalize => {
                return Ok(PlannedUnitPositionV1::Primary { phase, ordinal })
            }
            UnitPhase::OwnerShuffle | UnitPhase::BucketShuffle => {
                return self.run_span_at(phase, ordinal)
            }
            UnitPhase::FixedReduce => self.full_bottom_up_node_at(ordinal)?,
            UnitPhase::GratisPrefix if ordinal < primary => (0, ordinal),
            UnitPhase::GratisPrefix => self.active_bottom_up_node_at(ordinal - primary)?,
            UnitPhase::GratisPrefixDown if ordinal < active_internal => {
                self.active_top_down_node_at(ordinal)?
            }
            UnitPhase::GratisPrefixDown => (0, ordinal - active_internal),
            UnitPhase::RootReduce if ordinal < primary => (0, ordinal),
            UnitPhase::RootReduce => self.full_bottom_up_node_at(ordinal - primary)?,
        };
        Ok(PlannedUnitPositionV1::TreeNode {
            phase,
            level,
            index,
        })
    }

    fn run_span_at(
        self,
        phase: UnitPhase,
        ordinal: u32,
    ) -> Result<PlannedUnitPositionV1, PlannerErrorV1> {
        let primary = self.tree.primary_leaf_count;
        let (level, index) = if ordinal < primary {
            (0, ordinal)
        } else {
            self.shuffle_internal_node_at(ordinal - primary)?
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

    fn phase_ordinal_of(self, position: PlannedUnitPositionV1) -> Result<u32, PlannerErrorV1> {
        let primary = self.tree.primary_leaf_count;
        let phase = position.phase();
        let ordinal = match position {
            PlannedUnitPositionV1::Primary { ordinal, .. } => ordinal,
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::FixedReduce,
                level,
                index,
            } => self.full_bottom_up_ordinal(level, index)?,
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::GratisPrefix,
                level: 0,
                index,
            } => index,
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::GratisPrefix,
                level,
                index,
            } => primary
                .checked_add(self.active_bottom_up_ordinal(level, index)?)
                .ok_or(PlannerErrorV1::IntegerOverflow)?,
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::GratisPrefixDown,
                level: 0,
                index,
            } => self
                .active_internal_node_count()
                .checked_add(index)
                .ok_or(PlannerErrorV1::IntegerOverflow)?,
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::GratisPrefixDown,
                level,
                index,
            } => self.active_top_down_ordinal(level, index)?,
            PlannedUnitPositionV1::RunSpan {
                phase: UnitPhase::OwnerShuffle | UnitPhase::BucketShuffle,
                level: 0,
                index,
                ..
            } => index,
            PlannedUnitPositionV1::RunSpan {
                phase: UnitPhase::OwnerShuffle | UnitPhase::BucketShuffle,
                level,
                index,
                ..
            } => primary
                .checked_add(self.shuffle_internal_ordinal(level, index)?)
                .ok_or(PlannerErrorV1::IntegerOverflow)?,
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::RootReduce,
                level: 0,
                index,
            } => index,
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::RootReduce,
                level,
                index,
            } => primary
                .checked_add(self.full_bottom_up_ordinal(level, index)?)
                .ok_or(PlannerErrorV1::IntegerOverflow)?,
            _ => return Err(PlannerErrorV1::ProducerMembershipMismatch),
        };
        if self.phase_position_at(phase, ordinal)? != position {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        Ok(ordinal)
    }

    fn full_bottom_up_ordinal(self, level: u16, index: u32) -> Result<u32, PlannerErrorV1> {
        self.bottom_up_ordinal(level, index, |level| {
            Ok(self.tree.padded_leaf_count >> level)
        })
    }

    fn active_bottom_up_ordinal(self, level: u16, index: u32) -> Result<u32, PlannerErrorV1> {
        self.bottom_up_ordinal(level, index, |level| self.active_width(level))
    }

    fn bottom_up_ordinal(
        self,
        level: u16,
        index: u32,
        width_at: impl Fn(u16) -> Result<u32, PlannerErrorV1>,
    ) -> Result<u32, PlannerErrorV1> {
        if level == 0 || level > self.tree.height {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let mut ordinal = 0_u32;
        for current_level in 1..level {
            ordinal = ordinal
                .checked_add(width_at(current_level)?)
                .ok_or(PlannerErrorV1::IntegerOverflow)?;
        }
        if index >= width_at(level)? {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        ordinal
            .checked_add(index)
            .ok_or(PlannerErrorV1::IntegerOverflow)
    }

    fn active_top_down_ordinal(self, level: u16, index: u32) -> Result<u32, PlannerErrorV1> {
        if level == 0 || level > self.tree.height {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let mut ordinal = 0_u32;
        for current_level in ((level + 1)..=self.tree.height).rev() {
            ordinal = ordinal
                .checked_add(self.active_width(current_level)?)
                .ok_or(PlannerErrorV1::IntegerOverflow)?;
        }
        if index >= self.active_width(level)? {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        ordinal
            .checked_add(index)
            .ok_or(PlannerErrorV1::IntegerOverflow)
    }

    fn active_width(self, level: u16) -> Result<u32, PlannerErrorV1> {
        let width = 1_u32
            .checked_shl(u32::from(level))
            .ok_or(PlannerErrorV1::IntegerOverflow)?;
        Ok(self.tree.primary_leaf_count.div_ceil(width))
    }

    fn shuffle_internal_ordinal(self, level: u16, index: u32) -> Result<u32, PlannerErrorV1> {
        self.bottom_up_ordinal(level, index, |level| self.shuffle_node_count(level))
    }

    fn shuffle_node_count(self, level: u16) -> Result<u32, PlannerErrorV1> {
        let half_width = 1_u32
            .checked_shl(u32::from(level - 1))
            .ok_or(PlannerErrorV1::IntegerOverflow)?;
        let width = half_width
            .checked_mul(2)
            .ok_or(PlannerErrorV1::IntegerOverflow)?;
        self.tree
            .primary_leaf_count
            .checked_sub(1)
            .and_then(|value| value.checked_add(half_width))
            .ok_or(PlannerErrorV1::IntegerOverflow)
            .map(|value| value / width)
    }

    fn active_internal_node_count(self) -> u32 {
        (1..=self.tree.height)
            .map(|level| self.tree.primary_leaf_count.div_ceil(1_u32 << level))
            .sum()
    }

    fn shuffle_internal_node_at(self, mut ordinal: u32) -> Result<(u16, u32), PlannerErrorV1> {
        let primary = self.tree.primary_leaf_count;
        for level in 1..=self.tree.height {
            let half_width = 1_u32
                .checked_shl(u32::from(level - 1))
                .ok_or(PlannerErrorV1::IntegerOverflow)?;
            let width = half_width
                .checked_mul(2)
                .ok_or(PlannerErrorV1::IntegerOverflow)?;
            let node_count = primary
                .checked_sub(1)
                .and_then(|value| value.checked_add(half_width))
                .ok_or(PlannerErrorV1::IntegerOverflow)?
                / width;
            if ordinal < node_count {
                return Ok((level, ordinal));
            }
            ordinal -= node_count;
        }
        Err(PlannerErrorV1::IntegerOverflow)
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

    fn active_bottom_up_node_at(self, mut ordinal: u32) -> Result<(u16, u32), PlannerErrorV1> {
        for level in 1..=self.tree.height {
            let width = self.tree.primary_leaf_count.div_ceil(1_u32 << level);
            if ordinal < width {
                return Ok((level, ordinal));
            }
            ordinal -= width;
        }
        Err(PlannerErrorV1::IntegerOverflow)
    }

    fn active_top_down_node_at(self, mut ordinal: u32) -> Result<(u16, u32), PlannerErrorV1> {
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
