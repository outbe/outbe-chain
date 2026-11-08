use alloy_consensus::Transaction as _;
use reth_ethereum::TransactionSigned;

use super::{
    is_reserved_system_tx, BlockExecutionStage, BlockHeightRegime, BlockSystemPlan, BodyZone,
    SysTxSelector, SystemTx, SystemTxError, MAX_SYSTEM_TXS_PER_BLOCK,
};

/// Structural split of block transactions into system begin-prefix (`ConsensusStage` + `BeginBlock`),
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

    pub fn begin_block_selectors(&self) -> Result<Vec<SysTxSelector>, SystemTxError> {
        self.begin
            .iter()
            .map(|tx| SystemTx::decode((*tx).input()).map(|input| input.selector()))
            .collect()
    }

    pub fn end_block_selectors(&self) -> Result<Vec<SysTxSelector>, SystemTxError> {
        self.end
            .iter()
            .map(|tx| SystemTx::decode((*tx).input()).map(|input| input.selector()))
            .collect()
    }

    pub fn consensus_txs(&self) -> Result<Vec<&'a TransactionSigned>, SystemTxError> {
        let mut out = Vec::new();
        for tx in &self.begin {
            let decoded = SystemTx::decode((*tx).input())?;
            if decoded.stage() == BlockExecutionStage::ConsensusStage {
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

    /// True if the fully decoded begin zone contains a system tx with `selector`.
    pub fn has_begin_selector(&self, selector: SysTxSelector) -> bool {
        self.begin_block_selectors()
            .map(|selectors| selectors.contains(&selector))
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

pub fn expected_begin_block_selectors(
    block_number: u64,
    has_boundary_outcome: bool,
    has_tee_bootstrap: bool,
) -> Vec<SysTxSelector> {
    expected_begin_block_selectors_for_publication(
        block_number,
        has_boundary_outcome,
        has_tee_bootstrap,
        false,
    )
}

pub fn expected_begin_block_selectors_for_publication(
    block_number: u64,
    has_boundary_outcome: bool,
    has_tee_bootstrap: bool,
    has_publication: bool,
) -> Vec<SysTxSelector> {
    BlockSystemPlan::for_block(
        block_number,
        has_boundary_outcome,
        has_tee_bootstrap,
        has_publication,
    )
    .begin_zone_selectors()
}

pub fn expected_end_block_selectors(block_number: u64) -> Vec<SysTxSelector> {
    BlockSystemPlan::for_block(block_number, false, false, false).end_zone_selectors()
}

pub fn validate_active_system_tx_set(
    layout: &SystemTxLayout<'_>,
    block_number: u64,
    has_boundary_outcome: bool,
    has_tee_bootstrap: bool,
) -> Result<(), SystemTxError> {
    validate_system_tx_set_for_publication(
        layout,
        block_number,
        has_boundary_outcome,
        has_tee_bootstrap,
        false,
    )
}

pub fn validate_system_tx_set_for_publication(
    layout: &SystemTxLayout<'_>,
    block_number: u64,
    has_boundary_outcome: bool,
    has_tee_bootstrap: bool,
    has_publication: bool,
) -> Result<(), SystemTxError> {
    let actual = layout.system_tx_count();
    if actual > usize::from(MAX_SYSTEM_TXS_PER_BLOCK) {
        return Err(SystemTxError::TooManySystemTxs {
            actual,
            max: MAX_SYSTEM_TXS_PER_BLOCK,
        });
    }

    let regime = BlockHeightRegime::for_block(block_number);
    if regime.requires_genesis_bootstrap_txs() && !has_boundary_outcome {
        return Err(SystemTxError::V2Block1MissingBoundaryOutcome);
    }
    if regime.requires_genesis_bootstrap_txs() && !has_tee_bootstrap {
        return Err(SystemTxError::V2Block1MissingTeeBootstrap);
    }
    if !regime.requires_genesis_bootstrap_txs() && has_tee_bootstrap {
        return Err(SystemTxError::TeeBootstrapWrongHeight { block_number });
    }
    if !regime.allows_user_transactions() && !regime.is_genesis() && !layout.user.is_empty() {
        return Err(SystemTxError::V2Block1ContainsUserTransactions {
            actual: layout.user.len(),
        });
    }

    let expected_begin = expected_begin_block_selectors_for_publication(
        block_number,
        has_boundary_outcome,
        has_tee_bootstrap,
        has_publication,
    );
    let expected_end = expected_end_block_selectors(block_number);
    let actual_begin = layout.begin_block_selectors()?;
    let actual_end = layout.end_block_selectors()?;
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

fn ensure_system_tx_in_zone(input: &SystemTx, actual: BodyZone) -> Result<(), SystemTxError> {
    let expected = input.body_zone();
    if expected != actual {
        return Err(SystemTxError::SystemTxInWrongZone {
            kind: input.name(),
            expected,
            actual,
        });
    }
    Ok(())
}

fn ensure_monotonic(
    zone: BodyZone,
    previous: Option<(u8, &'static str)>,
    current: &SystemTx,
) -> Result<(u8, &'static str), SystemTxError> {
    let current_order = current
        .order_in(zone)
        .ok_or(SystemTxError::SystemTxInWrongZone {
            kind: current.name(),
            expected: current.body_zone(),
            actual: zone,
        })?;
    if let Some((previous_order, previous)) = previous {
        if current_order <= previous_order {
            return Err(SystemTxError::OutOfOrder {
                zone,
                previous,
                current: current.name(),
            });
        }
    }
    Ok((current_order, current.name()))
}
