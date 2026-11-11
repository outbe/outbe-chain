use crate::block::{BlockOrder, BlockSystemFlags};
use alloy_consensus::Transaction as _;
use reth_ethereum::TransactionSigned;

use super::{
    is_reserved_system_tx, BlockExecutionStage, BlockHeightRegime, BlockSystemPlan, BodyZone,
    SystemTx, SystemTxError, SystemTxStepId, CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX,
    MAX_END_BLOCK_SYSTEM_TXS, MAX_PREFIX_SYSTEM_TXS, MAX_SYSTEM_TXS_PER_BLOCK,
};

/// Structural split of block transactions into system begin-prefix (`Consensus` + `BeginBlock`),
/// user middle (`MidBlock`), and system end-suffix (`EndBlock`).
#[derive(Debug, Clone)]
pub struct SystemTxLayout<'a> {
    pub begin: Vec<&'a TransactionSigned>,
    pub user: Vec<&'a TransactionSigned>,
    pub end: Vec<&'a TransactionSigned>,
}

impl<'a> SystemTxLayout<'a> {
    pub fn is_empty(&self) -> bool {
        self.begin.is_empty() && self.user.is_empty() && self.end.is_empty()
    }

    pub fn system_tx_count(&self) -> usize {
        self.begin.len() + self.end.len()
    }

    pub fn prefix_steps(&self) -> Result<Vec<SystemTxStepId>, SystemTxError> {
        self.begin
            .iter()
            .map(|tx| SystemTx::decode((*tx).input()).map(|input| input.step_id()))
            .collect()
    }

    pub fn end_block_steps(&self) -> Result<Vec<SystemTxStepId>, SystemTxError> {
        self.end
            .iter()
            .map(|tx| SystemTx::decode((*tx).input()).map(|input| input.step_id()))
            .collect()
    }

    pub fn consensus_txs(&self) -> Result<Vec<&'a TransactionSigned>, SystemTxError> {
        let mut out = Vec::new();
        for tx in &self.begin {
            let decoded = SystemTx::decode((*tx).input())?;
            if decoded.stage() == BlockExecutionStage::Consensus {
                out.push(*tx);
            }
        }
        Ok(out)
    }

    pub fn begin_stage_txs(&self) -> Result<Vec<&'a TransactionSigned>, SystemTxError> {
        let mut out = Vec::new();
        for tx in &self.begin {
            let decoded = SystemTx::decode((*tx).input())?;
            if decoded.stage() == BlockExecutionStage::BeginBlock {
                out.push(*tx);
            }
        }
        Ok(out)
    }

    pub fn has_consensus_step(&self, step_index: u8) -> bool {
        self.prefix_steps()
            .map(|steps| steps.contains(&SystemTxStepId::consensus(step_index)))
            .unwrap_or(false)
    }
}

pub fn split_system_layout<'a>(
    txs: &'a [TransactionSigned],
) -> Result<SystemTxLayout<'a>, SystemTxError> {
    let mut begin = Vec::new();
    let mut prefix_end = 0usize;
    let mut previous_begin = None;

    while prefix_end < txs.len() && is_reserved_system_tx(&txs[prefix_end]) {
        let input = SystemTx::decode(txs[prefix_end].input())?;
        if input.body_zone() == BodyZone::EndBlock {
            break;
        }
        ensure_system_tx_in_zone(&input, BodyZone::BeginBlock)?;
        previous_begin = Some(ensure_monotonic(
            BodyZone::BeginBlock,
            previous_begin,
            &input,
        )?);
        begin.push(&txs[prefix_end]);
        prefix_end += 1;
    }

    let mut suffix_entries = Vec::new();
    let mut suffix_start = txs.len();
    while suffix_start > prefix_end && is_reserved_system_tx(&txs[suffix_start - 1]) {
        suffix_start -= 1;
        let input = SystemTx::decode(txs[suffix_start].input())?;
        ensure_system_tx_in_zone(&input, BodyZone::EndBlock)?;
        suffix_entries.push(suffix_start);
    }
    suffix_entries.reverse();

    let mut previous_end = None;
    let mut end = Vec::with_capacity(suffix_entries.len());
    for index in suffix_entries {
        let input = SystemTx::decode(txs[index].input())?;
        previous_end = Some(ensure_monotonic(BodyZone::EndBlock, previous_end, &input)?);
        end.push(&txs[index]);
    }

    for (offset, tx) in txs[prefix_end..suffix_start].iter().enumerate() {
        if is_reserved_system_tx(tx) {
            return Err(SystemTxError::MidBlockSystemTx {
                index: prefix_end + offset,
            });
        }
    }

    Ok(SystemTxLayout {
        begin,
        user: txs[prefix_end..suffix_start].iter().collect(),
        end,
    })
}

pub fn validate_system_tx_set_for_publication(
    order: &BlockOrder,
    layout: &SystemTxLayout<'_>,
    block_number: u64,
    flags: BlockSystemFlags,
) -> Result<(), SystemTxError> {
    let regime = BlockHeightRegime::for_block(block_number);
    if regime.has_system_tx_prefix() && order.consensus.is_empty() {
        return Err(SystemTxError::EmptyBlockOrder { block_number });
    }
    let actual = layout.system_tx_count();
    if actual > usize::from(MAX_SYSTEM_TXS_PER_BLOCK)
        || layout.begin.len() > MAX_PREFIX_SYSTEM_TXS
        || layout.end.len() > MAX_END_BLOCK_SYSTEM_TXS
    {
        return Err(SystemTxError::TooManySystemTxs {
            actual,
            max: MAX_SYSTEM_TXS_PER_BLOCK,
        });
    }
    validate_bootstrap_membership(layout, regime, block_number, flags)?;
    let plan = BlockSystemPlan::for_block(block_number, flags, order);
    let expected_begin = plan.expected_prefix_steps();
    let expected_end = plan.expected_end_block_steps();
    let actual_begin = layout.prefix_steps()?;
    let actual_end = layout.end_block_steps()?;
    if actual_begin != expected_begin || actual_end != expected_end {
        Err(SystemTxError::ActiveSystemTxSetMismatch {
            expected_begin,
            expected_end,
            actual_begin,
            actual_end,
        })
    } else {
        Ok(())
    }
}

fn validate_bootstrap_membership(
    layout: &SystemTxLayout<'_>,
    regime: BlockHeightRegime,
    block_number: u64,
    flags: BlockSystemFlags,
) -> Result<(), SystemTxError> {
    let has_tee_bootstrap = layout.has_consensus_step(CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX);
    if regime.requires_genesis_bootstrap_txs() && !flags.has_boundary_outcome {
        return Err(SystemTxError::V2Block1MissingBoundaryOutcome);
    }
    if regime.requires_genesis_bootstrap_txs() && !has_tee_bootstrap {
        return Err(SystemTxError::V2Block1MissingTeeBootstrap);
    }
    if !regime.requires_genesis_bootstrap_txs() && has_tee_bootstrap {
        return Err(SystemTxError::TeeBootstrapWrongHeight { block_number });
    }
    if regime.requires_genesis_bootstrap_txs() && !layout.user.is_empty() {
        return Err(SystemTxError::V2Block1ContainsUserTransactions {
            actual: layout.user.len(),
        });
    }
    Ok(())
}

fn ensure_system_tx_in_zone(input: &SystemTx, actual: BodyZone) -> Result<(), SystemTxError> {
    let expected = input.body_zone();
    if expected != actual {
        return Err(SystemTxError::SystemTxInWrongZone {
            step: input.step_id(),
            expected,
            actual,
        });
    }
    Ok(())
}

fn ensure_monotonic(
    zone: BodyZone,
    previous: Option<SystemTxStepId>,
    current: &SystemTx,
) -> Result<SystemTxStepId, SystemTxError> {
    ensure_system_tx_in_zone(current, zone)?;
    let current = current.step_id();
    if let Some(previous) = previous {
        if current <= previous {
            return Err(SystemTxError::OutOfOrder {
                zone,
                previous,
                current,
            });
        }
    }
    Ok(current)
}
