//! Build one bounded action page and its certified Merkle path.

use super::*;
use crate::lysis_result_catalog::VerifiedLysisResultChunkV1;

type BuildResult<T> = Result<T, NodMaterializationBuildErrorV1>;

pub(super) fn require_head_authority(
    audit: &LocalLysisPlanAuditV1<'_>,
    head: &NodMaterializationHeadV1,
) -> BuildResult<()> {
    let expected = (
        audit.plan().job_id,
        audit.bundle().bundle().lysis_program_semantics_hash,
        audit.plan().wwd,
        audit.plan().tribute_count,
    );
    let actual = (
        head.job_id,
        head.program_semantics_hash,
        head.worldwide_day,
        head.nod_count,
    );
    if actual != expected || head.next_nod_ordinal >= head.nod_count {
        return Err(NodMaterializationBuildErrorV1::AuthorityMismatch);
    }

    Ok(())
}

pub(super) struct MaterializationSubtree {
    tree_height: u16,
    height: u8,
    capacity: u32,
}

impl MaterializationSubtree {
    pub(super) fn new(head: &NodMaterializationHeadV1, configured: u8) -> BuildResult<Self> {
        let padded_count =
            head.nod_count
                .checked_next_power_of_two()
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "materialization padded NOD count",
                })?;
        let tree_height = padded_count.trailing_zeros() as u16;
        let height =
            aligned_subtree_height(head.next_nod_ordinal, configured).min(tree_height as u8);
        let capacity =
            1_u32
                .checked_shl(u32::from(height))
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "materialization batch capacity",
                })?;
        if capacity == 0 || capacity > PRIMARY_WORK_SHARD_SIZE {
            return Err(ProtocolError::InvalidInvariant("materialization batch capacity").into());
        }

        Ok(Self {
            tree_height,
            height,
            capacity,
        })
    }
}

pub(super) struct ActionPage {
    chunk_ordinal: u32,
    page_start: u32,
    local_start: u32,
    pub(super) actions: Vec<NodActionV1>,
    chunk: VerifiedLysisResultChunkV1,
}

impl ActionPage {
    pub(super) fn load(
        audit: &LocalLysisPlanAuditV1<'_>,
        head: &NodMaterializationHeadV1,
        subtree: &MaterializationSubtree,
    ) -> BuildResult<Self> {
        let chunk_ordinal = head.next_nod_ordinal / PRIMARY_WORK_SHARD_SIZE;
        let chunk = verified_result_chunk_at(audit, chunk_ordinal)?;
        let page_start = chunk_ordinal.checked_mul(PRIMARY_WORK_SHARD_SIZE).ok_or(
            ProtocolError::IntegerOverflow {
                what: "materialization page start",
            },
        )?;
        let local_start = head.next_nod_ordinal.checked_sub(page_start).ok_or(
            ProtocolError::InvalidInvariant("materialization chunk cursor"),
        )?;
        let action_count = head
            .nod_count
            .checked_sub(head.next_nod_ordinal)
            .ok_or(ProtocolError::InvalidInvariant(
                "materialization remaining NOD count",
            ))?
            .min(subtree.capacity);
        let local_end =
            local_start
                .checked_add(action_count)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "materialization page end",
                })?;
        let actions = chunk
            .chunk()
            .ordered_nod_actions
            .get(local_start as usize..local_end as usize)
            .ok_or(NodMaterializationBuildErrorV1::MissingActions)?
            .to_vec();
        require_action_ordinals(&actions, head.next_nod_ordinal, head.worldwide_day)?;
        Ok(ActionPage {
            chunk_ordinal,
            page_start,
            local_start,
            actions,
            chunk,
        })
    }
}

pub(super) struct PageTree {
    page_height: u16,
    levels: Vec<Vec<B256>>,
}

impl PageTree {
    pub(super) fn build(
        audit: &LocalLysisPlanAuditV1<'_>,
        head: &NodMaterializationHeadV1,
        subtree: &MaterializationSubtree,
        page: &ActionPage,
    ) -> BuildResult<Self> {
        let page_height = subtree
            .tree_height
            .min(PRIMARY_WORK_SHARD_SIZE.trailing_zeros() as u16);
        let page_capacity =
            1_u32
                .checked_shl(u32::from(page_height))
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "materialization page capacity",
                })?;
        let mut levels = vec![Vec::<B256>::new(); usize::from(page_height) + 1];
        levels[0].reserve(page_capacity as usize);
        for offset in 0..page_capacity {
            let ordinal =
                page.page_start
                    .checked_add(offset)
                    .ok_or(ProtocolError::IntegerOverflow {
                        what: "materialization page ordinal",
                    })?;
            let local = offset as usize;
            if let Some(action) = page.chunk.chunk().ordered_nod_actions.get(local) {
                if action.raw_ordinal != ordinal || action.wwd != head.worldwide_day {
                    return Err(NodMaterializationBuildErrorV1::ActionOrder);
                }
                levels[0].push(leaf_hash(
                    ListKind::NodActions,
                    ordinal,
                    &action.encode_canonical_record(audit.limits())?,
                )?);
            } else {
                levels[0].push(pad_hash(ListKind::NodActions, ordinal)?);
            }
        }
        for level in 1..=page_height {
            let previous = levels[usize::from(level - 1)].clone();
            let mut parents = Vec::with_capacity(previous.len() / 2);
            let global_start = page.page_start >> level;
            for (index, pair) in previous.as_chunks::<2>().0.iter().enumerate() {
                let global_index = global_start
                    .checked_add(u32::try_from(index).map_err(|_| {
                        ProtocolError::IntegerOverflow {
                            what: "materialization page node index",
                        }
                    })?)
                    .ok_or(ProtocolError::IntegerOverflow {
                        what: "materialization page node index",
                    })?;
                parents.push(node_hash(
                    ListKind::NodActions,
                    level,
                    global_index,
                    pair[0],
                    pair[1],
                )?);
            }
            levels[usize::from(level)] = parents;
        }

        Ok(Self {
            page_height,
            levels,
        })
    }
}

pub(super) struct MaterializationProof {
    pub(super) root_path: Vec<B256>,
    pub(super) dependencies: Vec<CasObjectRefV1>,
}

impl MaterializationProof {
    pub(super) fn build(
        audit: &LocalLysisPlanAuditV1<'_>,
        subtree: &MaterializationSubtree,
        page: &ActionPage,
        tree: &PageTree,
    ) -> BuildResult<Self> {
        let mut root_path =
            Vec::with_capacity(usize::from(subtree.tree_height - u16::from(subtree.height)));
        for child_level in u16::from(subtree.height)..tree.page_height {
            let local_index = (page.local_start >> child_level) as usize;
            root_path.push(
                *tree.levels[usize::from(child_level)]
                    .get(local_index ^ 1)
                    .ok_or(NodMaterializationBuildErrorV1::MissingSibling)?,
            );
        }

        let topology = LysisPlanTopologyV1::new(audit.plan().primary_work_unit_count)?;
        let mut dependencies = vec![
            page.chunk.producer_artifact_ref().clone(),
            page.chunk.output_manifest_entry().result_chunk_ref.clone(),
        ];
        let upper_path_levels = subtree.tree_height.checked_sub(tree.page_height).ok_or(
            ProtocolError::InvalidInvariant("materialization upper path height"),
        )?;
        for reducer_level in 0..upper_path_levels {
            let sibling_index = (page.chunk_ordinal >> reducer_level) ^ 1;
            let sibling_root =
                if reducer_level == 0 && sibling_index >= audit.plan().primary_work_unit_count {
                    LysisListSubtreeCarrierV1::canonical_empty_primary_page(
                        ListKind::NodActions,
                        sibling_index,
                    )?
                    .tree_root
                } else {
                    let position = PlannedUnitPositionV1::TreeNode {
                        phase: UnitPhase::RootReduce,
                        level: reducer_level,
                        index: sibling_index,
                    };
                    let ordinal = topology.plan_ordinal_of(position)?;
                    let artifact = audit.verified_artifact_at(ordinal)?;
                    let output = decode_root_reduce_output(
                        artifact.artifact().phase_payload(audit.limits())?,
                        audit.limits(),
                    )?;
                    let summary = match output {
                        RootReduceOutputV1::Leaf { summary, .. }
                        | RootReduceOutputV1::Node { summary } => summary,
                    };
                    let expected_height = (PRIMARY_WORK_SHARD_SIZE.trailing_zeros() as u16)
                        .checked_add(reducer_level)
                        .ok_or(ProtocolError::IntegerOverflow {
                            what: "materialization upper sibling height",
                        })?;
                    if summary.nod_actions.list_kind != ListKind::NodActions
                        || summary.nod_actions.subtree_height != expected_height
                        || summary.nod_actions.subtree_index != sibling_index
                    {
                        return Err(NodMaterializationBuildErrorV1::UpperSiblingMismatch);
                    }
                    dependencies.push(artifact.admission().artifact_ref.clone());
                    summary.nod_actions.tree_root
                };
            root_path.push(sibling_root);
        }
        normalize_dependencies(&mut dependencies)?;

        Ok(Self {
            root_path,
            dependencies,
        })
    }
}
