//! Outbe consensus builder and Reth consensus adapter.
//!
//! Outbe keeps the EVM timestamp in seconds, but stores a millisecond remainder
//! in [`OutbeHeader`]. Reth's stock `EthBeaconConsensus` validates parent/child
//! timestamp monotonicity through `BlockHeader::timestamp()` seconds, which
//! rejects valid sub-second Outbe blocks. Outbe also fixes a height-selected gas
//! schedule whose evidence-heavy block 1 deliberately expands beyond Ethereum's
//! parent/1024 ramp and contracts again at block 2. `OutbeBeaconConsensus`
//! delegates the remaining stock Ethereum checks and replaces those two parent
//! relations with their deterministic Outbe protocol rules.
//!
//! # V2 stateless layout / version / fork checks
//!
//! Beyond the timestamp adapter, `OutbeBeaconConsensus` runs the **stateless**
//! V2 system-transaction layout validator on every block:
//!
//! - reject legacy V1 selectors (`OSF1` / `OSC1` / `OSB1` / `OSO1`) at any
//!   height - V1 `FinalizationAndSlashing` is not silently dropped, it
//!   surfaces a typed error;
//! - reject malformed V2 envelopes: wrong `SYSTEM_TX_INPUT_VERSION` byte,
//!   unknown selector, missing body index 0 (`CertifiedParentAccounting`)
//!   for `block_number >= 2`, missing `BoundaryOutcome` for
//!   `block_number == 1`, any system tx in `block_number == 0`;
//! - enforce that the `CertifiedParentAccounting` metadata `finalized_block_hash`
//!   matches the header's `parent_hash` for `block_number >= 2`.
//!
//! Stateful BLS / VRF / accounting verification (BLS aggregate verify, VRF
//! proof verify, committee snapshot lookup, accounting progress comparison,
//! artifact hash compare, signer bitmap check) is **not** performed here. It
//! lives exclusively in `OutbeBlockExecutor::apply_pre_execution_changes`
//! (executor reorder task) so consensus pre-execution and execution
//! share a single stateful evaluator and cannot diverge.
//!
//! The integration-level pin for this stateless contract is
//! `crates/blockchain/node/tests/consensus_stateless.rs`.

use outbe_evm::system_tx::OcompLifecycleActivation;
use outbe_primitives::{
    addresses::REWARDS_ADDRESS, OutbeBlock, OutbeBlockBody, OutbeHeader, OutbePrimitives,
    OutbeReceipt,
};
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_ethereum::consensus::{
    Consensus, ConsensusError, EthBeaconConsensus, FullConsensus, HeaderValidator,
    ReceiptRootBloom, TransactionRoot,
};
use reth_execution_types::BlockExecutionResult;
use reth_node_builder::{
    components::ConsensusBuilder,
    node::{FullNodeTypes, NodeTypes},
    BuilderContext,
};
use reth_primitives_traits::{RecoveredBlock, SealedBlock, SealedHeader};
use std::{fmt::Debug, sync::Arc};

pub use outbe_primitives::consensus::OUTBE_MAX_EXTRA_DATA_SIZE;

mod policy;
mod system_transactions;

use policy::OutbeConsensusPolicy;

/// Build a `ConsensusError::Other` from a message string.
///
/// reth v2.2.0 changed `ConsensusError::Other` to carry
/// `Arc<dyn core::error::Error + Send + Sync>` instead of `String`, so the
/// message is wrapped in a boxed error first. Keeps all call sites terse and
/// avoids panics on the consensus path.
fn consensus_other(message: impl Into<String>) -> ConsensusError {
    ConsensusError::Other(Arc::<dyn core::error::Error + Send + Sync>::from(Box::<
        dyn core::error::Error + Send + Sync,
    >::from(
        message.into(),
    )))
}

/// Beacon consensus adapter that uses Outbe's full millisecond timestamp for
/// parent/child ordering while preserving Ethereum seconds semantics elsewhere.
#[derive(Debug, Clone)]
pub struct OutbeBeaconConsensus<ChainSpec> {
    inner: EthBeaconConsensus<ChainSpec>,
    policy: OutbeConsensusPolicy<ChainSpec>,
}

impl<ChainSpec> OutbeBeaconConsensus<ChainSpec>
where
    ChainSpec: EthChainSpec<Header = OutbeHeader> + EthereumHardforks,
{
    /// Create a new Outbe consensus adapter.
    pub fn new(chain_spec: Arc<ChainSpec>) -> Self {
        Self {
            inner: EthBeaconConsensus::new(chain_spec.clone()),
            policy: OutbeConsensusPolicy {
                chain_spec,
                skip_gas_limit_ramp_check: false,
                ocomp_lifecycle_activation: OcompLifecycleActivation::Disabled,
            },
        }
    }

    /// Returns the maximum allowed extra data size.
    pub const fn max_extra_data_size(&self) -> usize {
        self.inner.max_extra_data_size()
    }

    /// Sets the maximum allowed extra data size and returns the updated instance.
    pub fn with_max_extra_data_size(mut self, size: usize) -> Self {
        self.inner = self.inner.with_max_extra_data_size(size);
        self
    }

    /// Disables the gas limit change validation between parent and child blocks.
    pub fn with_skip_gas_limit_ramp_check(mut self, skip: bool) -> Self {
        self.inner = self.inner.with_skip_gas_limit_ramp_check(skip);
        self.policy.skip_gas_limit_ramp_check = skip;
        self
    }

    /// Disables the blob gas used check in header validation.
    pub fn with_skip_blob_gas_used_check(mut self, skip: bool) -> Self {
        self.inner = self.inner.with_skip_blob_gas_used_check(skip);
        self
    }

    /// Disables the requests hash check in post-execution validation.
    pub fn with_skip_requests_hash_check(mut self, skip: bool) -> Self {
        self.inner = self.inner.with_skip_requests_hash_check(skip);
        self
    }

    /// Installs structural OCOMP lifecycle activation. Normal construction is
    /// inert until OCM-26 wires the canonical fresh-devnet schedule.
    pub fn with_ocomp_lifecycle_activation(mut self, activation: OcompLifecycleActivation) -> Self {
        self.policy.ocomp_lifecycle_activation = activation;
        self
    }

    /// Returns the chain spec associated with this consensus engine.
    pub const fn chain_spec(&self) -> &Arc<ChainSpec> {
        &self.policy.chain_spec
    }
}

impl<ChainSpec> HeaderValidator<OutbeHeader> for OutbeBeaconConsensus<ChainSpec>
where
    ChainSpec: EthChainSpec<Header = OutbeHeader> + EthereumHardforks + Debug + Send + Sync,
{
    fn validate_header(&self, header: &SealedHeader<OutbeHeader>) -> Result<(), ConsensusError> {
        self.policy.validate_header(header.header())?;
        self.inner.validate_header(header)
    }

    fn validate_header_against_parent(
        &self,
        header: &SealedHeader<OutbeHeader>,
        parent: &SealedHeader<OutbeHeader>,
    ) -> Result<(), ConsensusError> {
        self.policy.validate_header_against_parent(header, parent)
    }
}

impl<ChainSpec> Consensus<OutbeBlock> for OutbeBeaconConsensus<ChainSpec>
where
    ChainSpec: EthChainSpec<Header = OutbeHeader> + EthereumHardforks + Debug + Send + Sync,
{
    fn validate_body_against_header(
        &self,
        body: &OutbeBlockBody,
        header: &SealedHeader<OutbeHeader>,
    ) -> Result<(), ConsensusError> {
        self.policy
            .validate_body_against_header(body, header.header())?;
        <EthBeaconConsensus<ChainSpec> as Consensus<OutbeBlock>>::validate_body_against_header(
            &self.inner,
            body,
            header,
        )
    }

    fn validate_block_pre_execution(
        &self,
        block: &SealedBlock<OutbeBlock>,
    ) -> Result<(), ConsensusError> {
        self.policy.validate_block_pre_execution(block)?;
        <EthBeaconConsensus<ChainSpec> as Consensus<OutbeBlock>>::validate_block_pre_execution(
            &self.inner,
            block,
        )
    }

    fn validate_block_pre_execution_with_tx_root(
        &self,
        block: &SealedBlock<OutbeBlock>,
        transaction_root: Option<TransactionRoot>,
    ) -> Result<(), ConsensusError> {
        self.policy.validate_block_pre_execution(block)?;
        <EthBeaconConsensus<ChainSpec> as Consensus<OutbeBlock>>::validate_block_pre_execution_with_tx_root(
            &self.inner,
            block,
            transaction_root,
        )
    }
}

impl<ChainSpec> FullConsensus<OutbePrimitives> for OutbeBeaconConsensus<ChainSpec>
where
    ChainSpec: EthChainSpec<Header = OutbeHeader> + EthereumHardforks + Debug + Send + Sync,
{
    fn validate_block_post_execution(
        &self,
        block: &RecoveredBlock<OutbeBlock>,
        result: &BlockExecutionResult<OutbeReceipt>,
        receipt_root_bloom: Option<ReceiptRootBloom>,
        block_access_list_hash: Option<alloy_primitives::B256>,
    ) -> Result<(), ConsensusError> {
        <EthBeaconConsensus<ChainSpec> as FullConsensus<OutbePrimitives>>::validate_block_post_execution(
            &self.inner,
            block,
            result,
            receipt_root_bloom,
            block_access_list_hash,
        )
    }
}

/// Stateless V2 system-transaction layout / version / fork validator.
///
/// Drives the `OutbeBeaconConsensus::validate_block_pre_execution` path and is
/// also exposed for integration coverage in
/// `crates/blockchain/node/tests/consensus_stateless.rs`. Stateful BLS / VRF /
/// accounting checks live in the EVM executor; see module docs.
pub fn validate_system_tx_consensus_boundary(
    body: &OutbeBlockBody,
    header: &OutbeHeader,
) -> Result<(), ConsensusError> {
    validate_system_tx_consensus_boundary_for_activation(
        body,
        header,
        OcompLifecycleActivation::Disabled,
    )
}

pub fn validate_system_tx_consensus_boundary_for_activation(
    body: &OutbeBlockBody,
    header: &OutbeHeader,
    ocomp_lifecycle_activation: OcompLifecycleActivation,
) -> Result<(), ConsensusError> {
    policy::validate_system_transactions(body, header, ocomp_lifecycle_activation)
}

/// Consensus builder that produces `OutbeBeaconConsensus` with increased extra_data limit.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct OutbeConsensusBuilder {
    ocomp_lifecycle_activation: OcompLifecycleActivation,
}

impl OutbeConsensusBuilder {
    #[must_use]
    pub const fn with_ocomp_lifecycle_activation(
        mut self,
        activation: OcompLifecycleActivation,
    ) -> Self {
        self.ocomp_lifecycle_activation = activation;
        self
    }
}

impl<Node> ConsensusBuilder<Node> for OutbeConsensusBuilder
where
    Node: FullNodeTypes<
        Types: NodeTypes<
            ChainSpec: EthChainSpec<Header = OutbeHeader> + EthereumHardforks,
            Primitives = OutbePrimitives,
        >,
    >,
{
    type Consensus = Arc<OutbeBeaconConsensus<<Node::Types as NodeTypes>::ChainSpec>>;

    async fn build_consensus(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Consensus> {
        Ok(Arc::new(
            OutbeBeaconConsensus::new(ctx.chain_spec())
                .with_max_extra_data_size(OUTBE_MAX_EXTRA_DATA_SIZE)
                .with_ocomp_lifecycle_activation(self.ocomp_lifecycle_activation),
        ))
    }
}

#[cfg(test)]
mod tests {
    mod adapter;

    use super::*;
    use alloy_consensus::{BlockHeader as _, Header};
    use alloy_eips::eip4895::{Withdrawal, Withdrawals};
    use alloy_primitives::{Address, Bloom, B256, B64, U256};
    use outbe_primitives::consensus::{
        MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS, MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS, OUTBE_MAX_BLOCK_SIZE,
    };
    use reth_chainspec::{ChainSpec, MAINNET};
    use reth_primitives_traits::Block as _;

    fn test_chain_spec() -> Arc<ChainSpec<OutbeHeader>> {
        MAINNET.as_ref().clone().map_header(OutbeHeader::new).into()
    }

    fn header(
        number: u64,
        timestamp_seconds: u64,
        timestamp_millis_part: u64,
        parent_hash: B256,
    ) -> SealedHeader<OutbeHeader> {
        header_with_beneficiary(
            number,
            (timestamp_seconds, timestamp_millis_part),
            parent_hash,
            if number == 0 {
                Address::ZERO
            } else {
                REWARDS_ADDRESS
            },
        )
    }

    fn header_with_beneficiary(
        number: u64,
        timestamp: (u64, u64),
        parent_hash: B256,
        beneficiary: Address,
    ) -> SealedHeader<OutbeHeader> {
        header_with_beneficiary_and_gas_limit(
            number,
            timestamp,
            parent_hash,
            HeaderOptions {
                beneficiary,
                gas_limit: outbe_primitives::system_tx::protocol_block_gas_limit(number),
            },
        )
    }

    struct HeaderOptions {
        beneficiary: Address,
        gas_limit: u64,
    }

    fn header_with_beneficiary_and_gas_limit(
        number: u64,
        timestamp: (u64, u64),
        parent_hash: B256,
        options: HeaderOptions,
    ) -> SealedHeader<OutbeHeader> {
        let (timestamp_seconds, timestamp_millis_part) = timestamp;
        let HeaderOptions {
            beneficiary,
            gas_limit,
        } = options;
        let extra_data = outbe_primitives::reshare_artifact::encode_outbe_block_artifacts(
            &outbe_primitives::reshare_artifact::OutbeBlockArtifacts {
                timestamp_millis_part,
                ..Default::default()
            },
        )
        .expect("encode artifacts");
        let header = OutbeHeader::new(Header {
            parent_hash,
            beneficiary,
            state_root: B256::ZERO,
            transactions_root: B256::ZERO,
            receipts_root: B256::ZERO,
            withdrawals_root: None,
            logs_bloom: Bloom::default(),
            number,
            gas_limit,
            gas_used: 0,
            timestamp: timestamp_seconds,
            mix_hash: B256::ZERO,
            base_fee_per_gas: None,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_block_root: None,
            requests_hash: None,
            block_access_list_hash: None,
            slot_number: None,
            extra_data,
            ommers_hash: alloy_consensus::EMPTY_OMMER_ROOT_HASH,
            difficulty: U256::ZERO,
            nonce: B64::ZERO,
        });
        SealedHeader::seal_slow(header)
    }

    fn phase1_metadata(
        block_number: u64,
        block_hash: B256,
    ) -> outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata {
        outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata {
            finalized_block_number: block_number,
            finalized_block_hash: block_hash,
            ..Default::default()
        }
    }

    fn signed_system_tx(
        signer: &outbe_evm::OutbeEvmSigner,
        ordinal: u8,
        block_number: u64,
        input: outbe_evm::system_tx::SystemTxInputV2,
    ) -> reth_ethereum::TransactionSigned {
        let unsigned = outbe_evm::system_tx::build_unsigned_system_tx(
            input.kind(),
            ordinal,
            block_number,
            MAINNET.chain().id(),
            input.encode().expect("system tx input encodes"),
        )
        .expect("system tx builds");
        signer.sign_unsigned(unsigned).expect("system tx signs")
    }

    fn body_with_withdrawal(amount: u64) -> OutbeBlockBody {
        OutbeBlockBody {
            transactions: Vec::new(),
            ommers: Vec::new(),
            withdrawals: Some(Withdrawals::new(vec![Withdrawal {
                index: 0,
                validator_index: 0,
                address: Address::ZERO,
                amount,
            }])),
        }
    }

    #[test]
    fn pre_execution_rejects_non_rewards_beneficiary() {
        let body = OutbeBlockBody {
            transactions: vec![signed_system_tx(
                &outbe_evm::OutbeEvmSigner::from_secret_bytes([4u8; 32]).unwrap(),
                0,
                1,
                outbe_evm::system_tx::SystemTxInputV2::CycleTick,
            )],
            ommers: Vec::new(),
            withdrawals: None,
        };
        let header = header_with_beneficiary(1, (100, 0), B256::ZERO, Address::ZERO)
            .header()
            .clone();

        let err = validate_system_tx_consensus_boundary(&body, &header).unwrap_err();

        assert!(matches!(
            err,
            ConsensusError::Other(message) if message.to_string().contains("beneficiary must be REWARDS_ADDRESS")
        ));
    }

    #[test]
    fn body_validation_rejects_non_empty_withdrawals() {
        let consensus = OutbeBeaconConsensus::new(test_chain_spec());
        let body = body_with_withdrawal(1_000);
        let header = header(0, 100, 0, B256::ZERO);

        let error = consensus
            .validate_body_against_header(&body, &header)
            .expect_err("Outbe must reject every non-empty withdrawals list");

        assert!(matches!(
            error,
            ConsensusError::Other(message)
                if message.to_string().contains("non-empty EIP-4895 withdrawals are unsupported on Outbe")
        ));
    }

    #[test]
    fn pre_execution_rejects_non_empty_withdrawals() {
        let consensus = OutbeBeaconConsensus::new(test_chain_spec());
        let sealed_header = header(0, 100, 0, B256::ZERO);
        let block = OutbeBlock {
            header: sealed_header.header().clone(),
            body: body_with_withdrawal(u64::MAX),
        }
        .seal_slow();

        let error = consensus
            .validate_block_pre_execution(&block)
            .expect_err("Outbe must reject every non-empty withdrawals list");

        assert!(matches!(
            error,
            ConsensusError::Other(message)
                if message.to_string().contains("non-empty EIP-4895 withdrawals are unsupported on Outbe")
        ));
    }

    #[test]
    fn pre_execution_with_tx_root_rejects_non_empty_withdrawals() {
        let consensus = OutbeBeaconConsensus::new(test_chain_spec());
        let sealed_header = header(0, 100, 0, B256::ZERO);
        let block = OutbeBlock {
            header: sealed_header.header().clone(),
            body: body_with_withdrawal(0),
        }
        .seal_slow();

        let error = consensus
            .validate_block_pre_execution_with_tx_root(&block, None)
            .expect_err("Outbe must reject even a zero-amount non-empty withdrawals list");

        assert!(matches!(
            error,
            ConsensusError::Other(message)
                if message.to_string().contains("non-empty EIP-4895 withdrawals are unsupported on Outbe")
        ));
    }

    #[test]
    fn pre_execution_rejects_finalization_metadata_for_non_parent_hash() {
        let signer = outbe_evm::OutbeEvmSigner::from_secret_bytes([3u8; 32]).unwrap();
        let parent_hash = B256::with_last_byte(0xAA);
        let wrong_parent_hash = B256::with_last_byte(0xBB);
        let phase1 = signed_system_tx(
            &signer,
            0,
            2,
            outbe_evm::system_tx::SystemTxInputV2::CertifiedParentAccounting {
                metadata: phase1_metadata(1, wrong_parent_hash),
            },
        );
        let late = signed_system_tx(
            &signer,
            1,
            2,
            outbe_evm::system_tx::SystemTxInputV2::LateFinalizeCredits {
                artifact: Default::default(),
            },
        );
        let cycle = signed_system_tx(
            &signer,
            2,
            2,
            outbe_evm::system_tx::SystemTxInputV2::CycleTick,
        );
        let rewards = signed_system_tx(
            &signer,
            3,
            2,
            outbe_evm::system_tx::SystemTxInputV2::RewardsGemDelivery,
        );
        let oracle = signed_system_tx(
            &signer,
            4,
            2,
            outbe_evm::system_tx::SystemTxInputV2::OracleSlashWindow,
        );
        let hook_events = signed_system_tx(
            &signer,
            5,
            2,
            outbe_evm::system_tx::SystemTxInputV2::HookEvents,
        );
        let body = OutbeBlockBody {
            transactions: vec![phase1, late, cycle, rewards, oracle, hook_events],
            ommers: Vec::new(),
            withdrawals: None,
        };
        let header = header(2, 100, 0, parent_hash).header().clone();

        let err = validate_system_tx_consensus_boundary(&body, &header).unwrap_err();

        assert!(
            matches!(err, ConsensusError::Other(message) if message.to_string().contains("CertifiedParentAccounting metadata hash must match block parent"))
        );
    }

    #[test]
    fn accepts_same_second_genesis_child_when_millis_increases() {
        // Proves the validator compares timestamps at millisecond granularity
        // (seconds*1000 + millis_part), not whole seconds: a same-UNIX-second
        // child with a higher millis part is monotonic and accepted. After
        // this sub-second advance is only valid at the genesis boundary (parent
        // block number 0), which is exempt from the minimum-advance bound; for
        // any real parent the same-second child is below the 1000 ms minimum and
        // correctly rejected (see `rejects_child_below_min_advance_timestamp_freeze`).
        let consensus = OutbeBeaconConsensus::new(test_chain_spec());
        let parent = header(0, 100, 900, B256::ZERO);
        let child = header(1, 100, 901, parent.hash());

        consensus
            .validate_header_against_parent(&child, &parent)
            .unwrap();
    }

    #[test]
    fn accepts_protocol_bootstrap_gas_limit_expansion_and_contraction() {
        use outbe_primitives::system_tx::{BOOTSTRAP_BLOCK_GAS_LIMIT, STEADY_BLOCK_GAS_LIMIT};

        let consensus = OutbeBeaconConsensus::new(test_chain_spec());
        let genesis = header_with_beneficiary_and_gas_limit(
            0,
            (100, 0),
            B256::ZERO,
            HeaderOptions {
                beneficiary: Address::ZERO,
                gas_limit: STEADY_BLOCK_GAS_LIMIT,
            },
        );
        let bootstrap = header_with_beneficiary_and_gas_limit(
            1,
            (101, 0),
            genesis.hash(),
            HeaderOptions {
                beneficiary: REWARDS_ADDRESS,
                gas_limit: BOOTSTRAP_BLOCK_GAS_LIMIT,
            },
        );
        let steady = header_with_beneficiary_and_gas_limit(
            2,
            (102, 0),
            bootstrap.hash(),
            HeaderOptions {
                beneficiary: REWARDS_ADDRESS,
                gas_limit: STEADY_BLOCK_GAS_LIMIT,
            },
        );

        consensus
            .validate_header_against_parent(&bootstrap, &genesis)
            .expect("the protocol-defined evidence-heavy block 1 must bypass the Ethereum ramp");
        consensus
            .validate_header_against_parent(&steady, &bootstrap)
            .expect("the protocol-defined steady block 2 must contract to its fixed limit");
    }

    #[test]
    fn rejects_gas_limit_not_selected_by_protocol_height() {
        use outbe_primitives::system_tx::STEADY_BLOCK_GAS_LIMIT;

        let consensus = OutbeBeaconConsensus::new(test_chain_spec());
        let genesis = header_with_beneficiary_and_gas_limit(
            0,
            (100, 0),
            B256::ZERO,
            HeaderOptions {
                beneficiary: Address::ZERO,
                gas_limit: STEADY_BLOCK_GAS_LIMIT,
            },
        );
        let wrong_bootstrap = header_with_beneficiary_and_gas_limit(
            1,
            (101, 0),
            genesis.hash(),
            HeaderOptions {
                beneficiary: REWARDS_ADDRESS,
                gas_limit: STEADY_BLOCK_GAS_LIMIT,
            },
        );

        let error = consensus
            .validate_header_against_parent(&wrong_bootstrap, &genesis)
            .expect_err("block 1 must use the protocol bootstrap gas limit");
        assert!(
            matches!(error, ConsensusError::Other(message) if message.to_string().contains("protocol gas limit"))
        );
    }

    #[test]
    fn rejects_child_when_millis_does_not_increase() {
        let consensus = OutbeBeaconConsensus::new(test_chain_spec());
        let parent = header(1, 100, 900, B256::ZERO);
        let child = header(2, 100, 900, parent.hash());

        let err = consensus
            .validate_header_against_parent(&child, &parent)
            .unwrap_err();

        assert!(matches!(
            err,
            ConsensusError::TimestampIsInPast {
                parent_timestamp: 100_900,
                timestamp: 100_900,
            }
        ));
    }

    #[test]
    fn accepts_next_second_child_with_zero_millis() {
        // A non-genesis child rolling forward to a later UNIX second with a zero
        // millis part validates: parent 100.999 s, child 102.000 s advances
        // 1001 ms, at or above the 1000 ms minimum. Exercises the
        // seconds + millis_part combine across a second boundary on the normal
        // (non-genesis) validation path.
        let consensus = OutbeBeaconConsensus::new(test_chain_spec());
        let parent = header(1, 100, 999, B256::ZERO);
        let child = header(2, 102, 0, parent.hash());

        consensus
            .validate_header_against_parent(&child, &parent)
            .unwrap();
    }

    #[test]
    fn outbe_genesis_keeps_paris_active_at_block_0() {
        // Test 14b: the real outbe genesis (terminalTotalDifficulty=0 +
        // terminalTotalDifficultyPassed, shanghai/cancun/prague Time=0) must keep
        // Paris/post-merge active at block 0. reth gates its wall-clock
        // future-timestamp check (`ConsensusError::TimestampIsInFuture`) behind the
        // pre-merge `else` of `is_paris_active_at_block`
        // (reth ethereum/consensus/src/lib.rs:163). With Paris active that branch is
        // dead, so a min-block-time-paced (delayed-emission) block can never trip a
        // wall-clock arrival bound. This regression catches a future chain-spec
        // change that might re-activate the pre-merge path.
        use reth_chainspec::EthereumHardforks;
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/assets/genesis.json");
        let bytes = std::fs::read(&path).expect("test genesis.json should be readable");
        let genesis: alloy_genesis::Genesis =
            serde_json::from_slice(&bytes).expect("test genesis.json should parse as Genesis");
        let chain_spec = ChainSpec::from(genesis);
        assert!(
            chain_spec.is_paris_active_at_block(0),
            "outbe genesis must keep Paris/post-merge active at block 0 so reth's \
             pre-merge future-timestamp check stays unreachable"
        );
    }

    #[test]
    fn accepts_paced_block_two_seconds_after_parent() {
        // A min-block-time-paced block is emitted ~2s after build, but its header
        // timestamp is fixed at build time (= max(now, parent + 1ms)). The validator
        // timestamp rule is a parent-relative increase with NO wall-clock/arrival
        // bound - only a deterministic max-drift band (`MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS`,
        // 1h) far above any paced interval - so a paced (delayed-emission) block
        // always validates and proposer pacing stays invisible to header validation.
        let consensus = OutbeBeaconConsensus::new(test_chain_spec());
        let parent = header(1, 100, 0, B256::ZERO);
        // +2000 ms relative to the parent (the default 2s floor), as +2 seconds.
        let child = header(2, 102, 0, parent.hash());

        consensus
            .validate_header_against_parent(&child, &parent)
            .unwrap();
    }

    #[test]
    fn rejects_header_with_invalid_millis_part() {
        let consensus = OutbeBeaconConsensus::new(test_chain_spec());
        let header = header(1, 100, 1000, B256::ZERO);

        let err = consensus.validate_header(&header).unwrap_err();

        assert!(
            matches!(err, ConsensusError::Other(message) if message.to_string().contains("timestamp_millis_part 1000"))
        );
    }

    #[test]
    fn accepts_child_at_max_drift_boundary() {
        // Parent at 100_000 ms; child exactly MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS
        // (3_600_000 ms = +3600 s) later is the largest accepted forward drift.
        let parent = header(1, 100, 0, B256::ZERO);
        let child = header(2, 100 + 3600, 0, parent.hash());
        assert_eq!(
            child.header().timestamp_millis() - parent.header().timestamp_millis(),
            MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS
        );
        OutbeBeaconConsensus::new(test_chain_spec())
            .validate_header_against_parent(&child, &parent)
            .unwrap();
    }

    #[test]
    fn rejects_child_one_milli_over_max_drift() {
        // One millisecond past the band must be rejected.
        let parent = header(1, 100, 0, B256::ZERO);
        let child = header(2, 100 + 3600, 1, parent.hash());
        assert_eq!(
            child.header().timestamp_millis() - parent.header().timestamp_millis(),
            MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS + 1
        );
        let err = OutbeBeaconConsensus::new(test_chain_spec())
            .validate_header_against_parent(&child, &parent)
            .unwrap_err();
        assert!(matches!(
            err,
            ConsensusError::Other(message) if message.to_string().contains("maximum drift")
        ));
    }

    #[test]
    fn rejects_far_future_timestamp_unbonding_bypass() {
        // C-01 regression: a byzantine proposer ratchets the timestamp 21 days
        // forward (the default unbonding period) to mature its own unbonding
        // entry and escape the slashing window in a single block. The drift
        // bound must reject it on every validator (chain-state only, no clock).
        let parent = header(10, 1_000_000, 0, B256::ZERO);
        let twenty_one_days_s = 21 * 24 * 3600;
        let child = header(11, 1_000_000 + twenty_one_days_s, 0, parent.hash());
        let err = OutbeBeaconConsensus::new(test_chain_spec())
            .validate_header_against_parent(&child, &parent)
            .unwrap_err();
        assert!(matches!(
            err,
            ConsensusError::Other(message) if message.to_string().contains("maximum drift")
        ));
    }

    #[test]
    fn accepts_child_at_min_advance_boundary() {
        // a non-genesis child advancing exactly MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS
        // (1000 ms = +1 s) over its parent is the smallest accepted advance.
        let parent = header(1, 100, 0, B256::ZERO);
        let child = header(2, 101, 0, parent.hash());
        assert_eq!(
            child.header().timestamp_millis() - parent.header().timestamp_millis(),
            MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS
        );
        OutbeBeaconConsensus::new(test_chain_spec())
            .validate_header_against_parent(&child, &parent)
            .unwrap();
    }

    #[test]
    fn rejects_child_below_min_advance_timestamp_freeze() {
        // regression: a colluding leader majority holds chain time near the
        // parent (here +999 ms, one below the 1000 ms minimum) to freeze
        // day-indexed emission and unbonding maturity. A non-genesis child below
        // the minimum advance must be rejected on every validator (chain-state
        // only, no clock).
        let parent = header(1, 100, 0, B256::ZERO);
        let child = header(2, 100, 999, parent.hash());
        assert_eq!(
            child.header().timestamp_millis() - parent.header().timestamp_millis(),
            MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS - 1
        );
        let err = OutbeBeaconConsensus::new(test_chain_spec())
            .validate_header_against_parent(&child, &parent)
            .unwrap_err();
        assert!(matches!(
            err,
            ConsensusError::Other(message) if message.to_string().contains("minimum advance")
        ));
    }

    #[test]
    fn genesis_child_exempt_from_min_advance() {
        // The genesis parent (block number 0) is exempt from the minimum-advance
        // bound: the proposer's `finalization_view` is unseeded at genesis, so
        // block 1 is monotonic-only on both proposer and validator paths. A
        // sub-minimum advance over genesis must still be accepted.
        let genesis = header(0, 100, 0, B256::ZERO);
        let block_one = header(1, 100, 999, genesis.hash());
        assert!(
            block_one.header().timestamp_millis() - genesis.header().timestamp_millis()
                < MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS
        );
        OutbeBeaconConsensus::new(test_chain_spec())
            .validate_header_against_parent(&block_one, &genesis)
            .unwrap();
    }
}
