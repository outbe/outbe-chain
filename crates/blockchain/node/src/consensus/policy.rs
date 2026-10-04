//! Stateless Outbe rules composed by the Reth consensus adapter.
use super::{consensus_other, system_transactions};
use alloy_consensus::BlockHeader as _;
use outbe_evm::system_tx::OcompLifecycleActivation;
use outbe_primitives::consensus::{
    MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS, MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS, OUTBE_MAX_BLOCK_SIZE,
};
use outbe_primitives::{
    payload::validate_outbe_withdrawals, OutbeBlock, OutbeBlockBody, OutbeHeader,
};
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_consensus_common::validation::{
    validate_against_parent_4844, validate_against_parent_eip1559_base_fee,
    validate_against_parent_hash_number,
};
use reth_ethereum::consensus::ConsensusError;
use reth_primitives_traits::{SealedBlock, SealedHeader};
use std::sync::Arc;

const MILLIS_PER_SECOND: u64 = 1000;

#[derive(Debug, Clone)]
pub(super) struct OutbeConsensusPolicy<ChainSpec> {
    pub(super) chain_spec: Arc<ChainSpec>,
    pub(super) skip_gas_limit_ramp_check: bool,
    pub(super) ocomp_lifecycle_activation: OcompLifecycleActivation,
}

impl<ChainSpec> OutbeConsensusPolicy<ChainSpec>
where
    ChainSpec: EthChainSpec<Header = OutbeHeader> + EthereumHardforks,
{
    pub(super) fn validate_header(&self, header: &OutbeHeader) -> Result<(), ConsensusError> {
        validate_header_timestamp_millis_part(header)?;
        if !self.skip_gas_limit_ramp_check {
            validate_protocol_gas_limit(header)?;
        }
        Ok(())
    }

    pub(super) fn validate_header_against_parent(
        &self,
        header: &SealedHeader<OutbeHeader>,
        parent: &SealedHeader<OutbeHeader>,
    ) -> Result<(), ConsensusError> {
        validate_against_parent_hash_number(header.header(), parent)?;
        validate_against_parent_timestamp_millis(header.header(), parent.header())?;

        if !self.skip_gas_limit_ramp_check {
            validate_protocol_gas_limit(header.header())?;
        }

        validate_against_parent_eip1559_base_fee(
            header.header(),
            parent.header(),
            self.chain_spec.as_ref(),
        )?;

        if let Some(blob_params) = self
            .chain_spec
            .blob_params_at_timestamp(header.header().timestamp())
        {
            validate_against_parent_4844(header.header(), parent.header(), blob_params)?;
        }

        Ok(())
    }

    pub(super) fn validate_body_against_header(
        &self,
        body: &OutbeBlockBody,
        header: &OutbeHeader,
    ) -> Result<(), ConsensusError> {
        validate_outbe_body_withdrawals(body)?;
        validate_system_transactions(body, header, self.ocomp_lifecycle_activation)
    }

    pub(super) fn validate_block_pre_execution(
        &self,
        block: &SealedBlock<OutbeBlock>,
    ) -> Result<(), ConsensusError> {
        validate_outbe_body_withdrawals(block.body())?;
        validate_block_transport_size(block)?;
        validate_system_transactions(
            block.body(),
            block.header(),
            self.ocomp_lifecycle_activation,
        )
    }
}

pub(super) fn validate_system_transactions(
    body: &OutbeBlockBody,
    header: &OutbeHeader,
    ocomp_lifecycle_activation: OcompLifecycleActivation,
) -> Result<(), ConsensusError> {
    system_transactions::validate_beneficiary(header)?;
    let (layout, artifacts) =
        system_transactions::validate_layout(body, header, ocomp_lifecycle_activation)?;
    system_transactions::validate_parent_accounting(&layout, header)?;
    system_transactions::validate_boundary_outcome(&layout, &artifacts)?;
    system_transactions::validate_late_credits(&layout, &artifacts)
}

/// Enforce the genesis-fixed Outbe gas schedule by height.
///
/// Ethereum's parent-relative ramp cannot represent the intentional 30M -> 500M
/// block-1 bootstrap expansion or the 500M -> 30M block-2 contraction. Exact
/// height selection is stronger here: a proposer cannot choose any intermediate
/// or oversized value, and every node derives the same limit without parent or
/// host input.
fn validate_protocol_gas_limit(header: &OutbeHeader) -> Result<(), ConsensusError> {
    let expected = outbe_primitives::system_tx::protocol_block_gas_limit(header.number());
    let actual = header.gas_limit();
    if actual != expected {
        return Err(consensus_other(format!(
            "block {} protocol gas limit mismatch: expected {expected}, got {actual}",
            header.number()
        )));
    }
    Ok(())
}

fn validate_outbe_body_withdrawals(body: &OutbeBlockBody) -> Result<(), ConsensusError> {
    validate_outbe_withdrawals(
        body.withdrawals
            .as_ref()
            .map(|withdrawals| withdrawals.0.as_slice()),
    )
    .map_err(|error| consensus_other(error.to_string()))
}

/// Reject a block whose RLP-encoded size exceeds the consensus P2P transport
/// cap (`OUTBE_MAX_BLOCK_SIZE`). Deterministic (RLP length of the same sealed
/// block on every validator), so an over-sized byzantine block is rejected
/// here rather than panicking commonware's bounded sender on dissemination.
/// Honest proposers cap the block at build time, so this never rejects a valid
/// block. See README "Consensus Artifact Transport".
fn validate_block_transport_size(block: &SealedBlock<OutbeBlock>) -> Result<(), ConsensusError> {
    let rlp_length = block.rlp_length();
    if rlp_length > OUTBE_MAX_BLOCK_SIZE {
        return Err(consensus_other(format!(
            "block RLP size {rlp_length} exceeds the {OUTBE_MAX_BLOCK_SIZE}-byte P2P transport cap"
        )));
    }
    Ok(())
}

fn validate_against_parent_timestamp_millis(
    header: &OutbeHeader,
    parent: &OutbeHeader,
) -> Result<(), ConsensusError> {
    let timestamp = header.timestamp_millis();
    let parent_timestamp = parent.timestamp_millis();

    if timestamp <= parent_timestamp {
        return Err(ConsensusError::TimestampIsInPast {
            parent_timestamp,
            timestamp,
        });
    }

    // Upper bound on forward drift. Stock Ethereum only checks monotonicity,
    // which lets a single byzantine proposer ratchet chain time arbitrarily far
    // forward in one block - maturing every unbonding entry and the slashed
    // withdrawal delay (unbonding-lock + slashing-window bypass) and skipping
    // the day-indexed emission schedule. The bound is deterministic and
    // chain-state-only (header + parent, no wall clock), so proposer and every
    // validator agree. Honest proposers cap their assigned timestamp at
    // `parent + MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS` (see the consensus handler
    // build path), so this never rejects an honest block; a long outage
    // self-heals as chain time ratchets forward in bounded steps.
    let drift = timestamp - parent_timestamp;
    if drift > MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS {
        return Err(consensus_other(format!(
            "block timestamp_millis {timestamp} exceeds parent {parent_timestamp} by {drift} ms, \
             above the {MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS} ms maximum drift"
        )));
    }

    // Lower bound on forward advance. Monotonicity alone lets a colluding
    // leader majority hold `timestamp = parent + 1 ms` while real time advances,
    // freezing day-indexed emission and unbonding maturity. Each non-genesis
    // block must advance chain time by at least `MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS`.
    // Deterministic and chain-state-only; the proposer clamps its assigned
    // timestamp up to `parent + this` (see the consensus handler build path) so an
    // honest block is never rejected. The genesis child (parent number 0) is
    // exempt - its `finalization_view` is unseeded, so block 1 is monotonic-only,
    // matching the proposer's genesis exception.
    if parent.number() > 0 && drift < MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS {
        return Err(consensus_other(format!(
            "block timestamp_millis {timestamp} advances parent {parent_timestamp} by only \
             {drift} ms, below the {MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS} ms minimum advance"
        )));
    }

    Ok(())
}

fn validate_header_timestamp_millis_part(header: &OutbeHeader) -> Result<(), ConsensusError> {
    let part = header.timestamp_millis_part();
    if part >= MILLIS_PER_SECOND {
        return Err(consensus_other(format!(
            "timestamp_millis_part {part} must be less than {MILLIS_PER_SECOND}"
        )));
    }

    Ok(())
}
