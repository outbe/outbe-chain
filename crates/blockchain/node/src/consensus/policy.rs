//! Stateless Outbe rules composed by the Reth consensus adapter.
use super::{consensus_other, system_transactions};
use alloy_consensus::BlockHeader as _;
use outbe_primitives::consensus::{
    classify_timestamp_advance, TimestampAdvance, MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS,
    MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS, OUTBE_MAX_BLOCK_SIZE,
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
            .get(&outbe_primitives::addresses::CODE_REGISTRY_ADDRESS)
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
        let actual = if header.extra_data().starts_with(b"OART") {
            outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(header.extra_data())
                .map_err(|error| consensus_other(error.to_string()))?
                .radicle_publication_enabled
        } else {
            false
        };
        if actual != enabled {
            return Err(consensus_other(
                "OART publication format differs from seeded genesis activation",
            ));
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
    // Upper drift stops one proposer from jumping chain time. The minimum
    // advance stops a colluding majority from freezing time at parent + 1 ms.
    // Parent block number 0 skips only the minimum. A parent timestamp of 0
    // does not skip the upper bound.
    match classify_timestamp_advance(
        parent_timestamp,
        timestamp,
        MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS,
        MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS,
    ) {
        TimestampAdvance::NotAfterParent => Err(ConsensusError::TimestampIsInPast {
            parent_timestamp,
            timestamp,
        }),
        TimestampAdvance::ExceedsMaxDrift { drift } => Err(
            crate::errors::ConsensusPolicyError::TimestampExceedsMaxDrift {
                timestamp,
                parent: parent_timestamp,
                drift,
                max_drift: MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS,
            }
            .into(),
        ),
        TimestampAdvance::BelowMinAdvance { drift } if parent.number() > 0 => Err(
            crate::errors::ConsensusPolicyError::TimestampBelowMinAdvance {
                timestamp,
                parent: parent_timestamp,
                drift,
                min_advance: MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS,
            }
            .into(),
        ),
        TimestampAdvance::BelowMinAdvance { .. } | TimestampAdvance::Inside { .. } => Ok(()),
    }
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

#[cfg(test)]
mod timestamp_domain_tests {
    use super::validate_against_parent_timestamp_millis;
    use alloy_consensus::Header;
    use outbe_primitives::consensus::{
        classify_timestamp_advance, TimestampAdvance, MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS,
        MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS,
    };
    use outbe_primitives::reshare_artifact::{encode_outbe_block_artifacts, OutbeBlockArtifacts};
    use outbe_primitives::OutbeHeader;
    use reth_ethereum::consensus::ConsensusError;

    fn header_millis(number: u64, timestamp_millis: u64) -> OutbeHeader {
        let (seconds, part) = OutbeHeader::split_timestamp_millis(timestamp_millis);
        OutbeHeader::new(Header {
            number,
            timestamp: seconds,
            extra_data: encode_outbe_block_artifacts(&OutbeBlockArtifacts {
                timestamp_millis_part: part,
                ..Default::default()
            })
            .expect("timestamp artifact encodes"),
            ..Default::default()
        })
    }

    #[test]
    fn parent_number_zero_skips_only_the_minimum_advance() {
        let parent_timestamp = 1_700_000_000_000;
        let short = parent_timestamp + 1;
        assert_eq!(
            classify_timestamp_advance(
                parent_timestamp,
                short,
                MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS,
                MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS,
            ),
            TimestampAdvance::BelowMinAdvance { drift: 1 }
        );
        assert!(validate_against_parent_timestamp_millis(
            &header_millis(1, short),
            &header_millis(0, parent_timestamp),
        )
        .is_ok());
        let rejected = validate_against_parent_timestamp_millis(
            &header_millis(2, short),
            &header_millis(1, parent_timestamp),
        )
        .expect_err("a later parent keeps the minimum advance");
        assert!(!matches!(
            rejected,
            ConsensusError::TimestampIsInPast { .. }
        ));
        assert!(rejected
            .to_string()
            .contains(&MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS.to_string()));

        let over = parent_timestamp + MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS + 1;
        let over_number_zero = validate_against_parent_timestamp_millis(
            &header_millis(1, over),
            &header_millis(0, parent_timestamp),
        )
        .expect_err("parent number 0 still enforces the upper drift");
        assert!(over_number_zero
            .to_string()
            .contains(&MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS.to_string()));
        assert_ne!(rejected.to_string(), over_number_zero.to_string());

        let zero_parent_timestamp = validate_against_parent_timestamp_millis(
            &header_millis(1, MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS + 1),
            &header_millis(0, 0),
        )
        .expect_err("a parent timestamp of 0 is not an upper-bound exemption");
        assert!(matches!(
            classify_timestamp_advance(
                0,
                MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS + 1,
                MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS,
                MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS,
            ),
            TimestampAdvance::ExceedsMaxDrift { .. }
        ));
        assert!(zero_parent_timestamp
            .to_string()
            .contains(&MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS.to_string()));
    }
}
