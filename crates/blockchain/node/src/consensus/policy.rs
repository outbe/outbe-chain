//! Stateless Outbe rules composed by the Reth consensus adapter.
use super::{consensus_other, system_transactions};
use alloy_consensus::BlockHeader as _;
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

#[cfg(test)]
mod publication_tests;

#[derive(Debug, Clone)]
pub(super) struct OutbeConsensusPolicy<ChainSpec> {
    pub(super) chain_spec: Arc<ChainSpec>,
    pub(super) skip_gas_limit_ramp_check: bool,
}

impl<ChainSpec> OutbeConsensusPolicy<ChainSpec>
where
    ChainSpec: EthChainSpec<Header = OutbeHeader> + EthereumHardforks,
{
    fn validate_repository_publication_mode(
        &self,
        header: &OutbeHeader,
    ) -> Result<(), ConsensusError> {
        if header.number() == 0 {
            return Ok(());
        }
        use outbe_primitives::radicle_genesis::{
            GovernmentsDescriptor, GENESIS_GOVERNMENTS_FIELD, GOVERNMENTS_DESCRIPTOR_HASH_SLOT,
        };
        let genesis = self.chain_spec.genesis();
        let slot = alloy_primitives::B256::from(
            alloy_primitives::U256::from(GOVERNMENTS_DESCRIPTOR_HASH_SLOT).to_be_bytes::<32>(),
        );
        let seeded_hash = genesis
            .alloc
            .get(&outbe_primitives::addresses::RADICLE_REGISTRY_ADDRESS)
            .and_then(|account| account.storage.as_ref())
            .and_then(|storage| storage.get(&slot))
            .copied()
            .unwrap_or_default();
        let enabled = GovernmentsDescriptor::from_genesis_field(
            genesis.config.extra_fields.get(GENESIS_GOVERNMENTS_FIELD),
            self.chain_spec.chain().id(),
            genesis.timestamp,
            seeded_hash,
        )
        .map_err(|error| consensus_other(error.to_string()))?
        .is_some();
        let actual = header.extra_data().starts_with(b"OART\x0c");
        if actual != enabled {
            return Err(consensus_other(
                "OART publication format differs from seeded genesis activation",
            ));
        }
        if enabled {
            outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(header.extra_data())
                .map_err(|error| consensus_other(error.to_string()))?;
        }
        Ok(())
    }

    pub(super) fn validate_header(&self, header: &OutbeHeader) -> Result<(), ConsensusError> {
        self.validate_repository_publication_mode(header)?;
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
        self.validate_repository_publication_mode(header)?;
        validate_outbe_body_withdrawals(body)?;
        validate_system_transactions(body, header)
    }

    pub(super) fn validate_block_pre_execution(
        &self,
        block: &SealedBlock<OutbeBlock>,
    ) -> Result<(), ConsensusError> {
        self.validate_repository_publication_mode(block.header())?;
        validate_outbe_body_withdrawals(block.body())?;
        validate_block_transport_size(block)?;
        validate_system_transactions(block.body(), block.header())
    }
}

pub(super) fn validate_system_transactions(
    body: &OutbeBlockBody,
    header: &OutbeHeader,
) -> Result<(), ConsensusError> {
    system_transactions::validate_beneficiary(header)?;
    let (layout, artifacts) = system_transactions::validate_layout(body, header)?;
    system_transactions::validate_parent_accounting(&layout, header)?;
    system_transactions::validate_boundary_outcome(&layout, &artifacts)?;
    system_transactions::validate_late_credits(&layout, &artifacts)
}

/// Enforce the genesis-fixed Outbe gas schedule by height.
///
/// Ethereum's parent-relative ramp cannot represent the intentional 30M -> 500M
/// block-1 bootstrap expansion or the 500M -> 30M block-2 contraction. Exact
/// height selection is stronger here. A proposer cannot choose any intermediate
/// or oversized value. Every node derives the same limit without parent or host
/// input.
fn validate_protocol_gas_limit(header: &OutbeHeader) -> Result<(), ConsensusError> {
    let expected = outbe_primitives::system_tx::protocol_block_gas_limit(header.number());
    let actual = header.gas_limit();
    if actual != expected {
        return Err(
            crate::errors::ConsensusPolicyError::ProtocolGasLimitMismatch {
                block_number: header.number(),
                expected,
                actual,
            }
            .into(),
        );
    }
    Ok(())
}

fn validate_outbe_body_withdrawals(body: &OutbeBlockBody) -> Result<(), ConsensusError> {
    validate_outbe_withdrawals(
        body.withdrawals
            .as_ref()
            .map(|withdrawals| withdrawals.0.as_slice()),
    )
    .map_err(crate::errors::consensus_display_error)
}

/// Reject a block whose RLP-encoded size exceeds the consensus P2P transport
/// cap (`OUTBE_MAX_BLOCK_SIZE`). The check is deterministic: every validator
/// measures the RLP length of the same sealed block. Thus this check rejects an
/// over-sized byzantine block here, before the block can panic commonware's
/// bounded sender on dissemination. Honest proposers cap the block at build
/// time, so this never rejects a valid block. See README "Consensus Artifact
/// Transport".
fn validate_block_transport_size(block: &SealedBlock<OutbeBlock>) -> Result<(), ConsensusError> {
    let rlp_length = block.rlp_length();
    if rlp_length > OUTBE_MAX_BLOCK_SIZE {
        return Err(
            crate::errors::ConsensusPolicyError::BlockRlpSizeExceedsTransportCap {
                rlp_length,
                max_block_rlp_bytes: OUTBE_MAX_BLOCK_SIZE,
            }
            .into(),
        );
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

    // Upper bound on forward drift. Stock Ethereum only checks monotonicity.
    // That lets a single byzantine proposer ratchet chain time arbitrarily far
    // forward in one block. Such a block:
    // - matures every unbonding entry and the slashed withdrawal delay
    //   (unbonding-lock + slashing-window bypass).
    // - skips the day-indexed emission schedule.
    // The bound is deterministic and chain-state-only (header + parent, no wall
    // clock), so proposer and every validator agree. Honest proposers cap their
    // assigned timestamp at `parent + MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS` (see the
    // consensus handler build path), so this never rejects an honest block. A
    // long outage self-heals as chain time ratchets forward in bounded steps.
    let drift = timestamp - parent_timestamp;
    if drift > MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS {
        return Err(
            crate::errors::ConsensusPolicyError::TimestampExceedsMaxDrift {
                timestamp,
                parent: parent_timestamp,
                drift,
                max_drift: MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS,
            }
            .into(),
        );
    }

    // Lower bound on forward advance. Monotonicity alone lets a colluding
    // leader majority hold `timestamp = parent + 1 ms` while real time advances.
    // That freezes day-indexed emission and unbonding maturity. Each non-genesis
    // block must advance chain time by at least `MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS`.
    // The bound is deterministic and chain-state-only. The proposer clamps its
    // assigned timestamp up to `parent + this` (see the consensus handler build
    // path), so this check never rejects an honest block. The genesis child
    // (parent number 0) is exempt. Its `finalization_view` is unseeded, so block 1
    // is monotonic-only, matching the proposer's genesis exception.
    if parent.number() > 0 && drift < MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS {
        return Err(
            crate::errors::ConsensusPolicyError::TimestampBelowMinAdvance {
                timestamp,
                parent: parent_timestamp,
                drift,
                min_advance: MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS,
            }
            .into(),
        );
    }

    Ok(())
}

fn validate_header_timestamp_millis_part(header: &OutbeHeader) -> Result<(), ConsensusError> {
    let part = header.timestamp_millis_part();
    if part >= MILLIS_PER_SECOND {
        return Err(
            crate::errors::ConsensusPolicyError::InvalidTimestampMillisPart {
                part,
                max_part: MILLIS_PER_SECOND,
            }
            .into(),
        );
    }

    Ok(())
}
