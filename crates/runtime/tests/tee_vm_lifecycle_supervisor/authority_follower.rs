//! Preload a recovered anchor, then build and import through production execution.
use crate::{execution_committee::Committee, rejoin_execution::Ledger, TEST_CHAIN_ID};
use alloy_consensus::Header;
use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
use alloy_primitives::{Address, Bytes, B256, U256};
use outbe_evm::{EvmRuntimeHost as _, OutbeEvmConfig, OutbeNextBlockEnvAttributes};
use outbe_primitives::{
    addresses::REWARDS_ADDRESS,
    consensus_metadata::CertifiedParentAccountingMetadata,
    reshare_artifact::{
        encode_outbe_block_artifacts, ExecutionSummaryArtifact, OutbeBlockArtifacts,
    },
    system_tx::protocol_block_gas_limit,
    OutbeBlock, OutbeHeader,
};
use outbe_validatorset::{contract::ValidatorSet, CommitteeSnapshot};
use reth_chainspec::ChainSpec;
use reth_ethereum::TransactionSigned;
use reth_evm::database::StateProviderDatabase;
use reth_evm::{execute::BlockBuilder, ConfigureEvm, NextBlockEnvAttributes};
use reth_primitives_traits::{Recovered, SealedHeader};
use reth_storage_api::{EvmStateProviderAdapter, StateProviderFactory};

pub(super) fn chain_spec() -> ChainSpec<OutbeHeader> {
    let mut genesis = alloy_genesis::Genesis {
        timestamp: 1,
        gas_limit: protocol_block_gas_limit(0),
        base_fee_per_gas: Some(u128::from(MIN_PROTOCOL_BASE_FEE)),
        ..Default::default()
    };
    genesis.config.chain_id = TEST_CHAIN_ID;
    reth_chainspec::ChainSpecBuilder::default()
        .chain(TEST_CHAIN_ID.into())
        .genesis(genesis)
        .shanghai_activated()
        .build()
        .map_header(OutbeHeader::new)
}

pub(super) fn seed_execution_committee(
    ledger: &mut Ledger,
    committee: &Committee,
    (proposer, attestation): (Address, [u8; 32]),
    active_resident: bool,
) -> CommitteeSnapshot {
    let mut snapshot = committee.snapshot([Address::ZERO; 4]);
    let resident_key = crate::finality::FinalityCommitteeFixture::new(10).public_keys_min_pk()[0];
    let resident = crate::Signer::address();
    let mut peers = [
        proposer,
        Address::repeat_byte(0xa1),
        Address::repeat_byte(0xa2),
    ]
    .into_iter();
    for member in &mut snapshot.committee {
        member.address = if member.consensus_pubkey == resident_key {
            resident
        } else {
            peers.next().unwrap()
        };
    }
    ledger.storage(|storage| {
        let owner = Address::repeat_byte(0xa0);
        let mut validators = ValidatorSet::new(storage.clone());
        validators.config_is_initialized.write(true).unwrap();
        validators.test_configure_registry(owner).unwrap();
        validators.config_epoch_length_blocks.write(60).unwrap();
        let minimum = U256::from(1_000);
        validators
            .test_seed_authoritative_min_stake(minimum)
            .unwrap();
        let registry = outbe_teeregistry::TeeRegistry::new(storage.clone());
        let mut total = U256::ZERO;
        for (index, member) in snapshot.committee.iter().enumerate() {
            let address = member.address;
            validators
                .register_validator(owner, address, &member.consensus_pubkey)
                .unwrap();
            // A pending resident reaches lease expiry without entering the active-set jail sweep.
            if address != resident || active_resident {
                validators
                    .test_seed_authoritative_bonded(address, minimum)
                    .unwrap();
                validators
                    .activate_validator_via_boundary_for_test(address)
                    .unwrap();
                total += minimum;
            }
            if address != resident {
                let node = B256::with_last_byte(0xa4 + index as u8);
                registry
                    .validator_v1_node_hash
                    .write(&address, node)
                    .unwrap();
                registry.v1_node_intent_hash.write(&node, node).unwrap();
                registry.v1_node_valid_until.write(&node, u64::MAX).unwrap();
                if address == proposer {
                    registry
                        .v1_node_attestation_ed25519
                        .write(&node, B256::from(attestation))
                        .unwrap();
                }
            }
        }
        let staking = outbe_staking::contract::Staking::new(storage.clone());
        staking.total_staked.write(total).unwrap();
        storage
            .increase_balance(outbe_system_common::addresses::STAKING_ADDRESS, total)
            .unwrap();
        assert!(validators.is_consensus_participant(proposer).unwrap());
        outbe_validatorset::write_committee_snapshot(storage, 0, &snapshot).unwrap();
    });
    snapshot
}

pub(super) fn seed_recovered_state(ledger: &mut Ledger) {
    let last_accounted = ledger.tip.height;
    ledger.storage(|storage| {
        let marker = evm2::bytecode::Bytecode::new_legacy(Bytes::from_static(&[0xef]));
        for &address in outbe_runtime::OutbeRuntime::new().marker_addresses() {
            storage.set_code(address, marker.clone()).unwrap();
        }
        storage
            .contract::<outbe_cycle::schema::Accounting<'_>>()
            .last_accounted_block_number
            .write(last_accounted)
            .unwrap();
    });
}

pub(super) fn recovered_headers(ledger: &mut Ledger, genesis_hash: B256) -> Vec<OutbeBlock> {
    let mut parent = genesis_hash;
    let mut blocks = Vec::new();
    for number in 1..ledger.tip.height {
        let block = OutbeBlock {
            header: OutbeHeader::new(Header {
                number,
                parent_hash: parent,
                timestamp: (ledger.tip.timestamp / ledger.tip.height) * number,
                gas_limit: protocol_block_gas_limit(number),
                base_fee_per_gas: Some(MIN_PROTOCOL_BASE_FEE),
                withdrawals_root: Some(alloy_trie::EMPTY_ROOT_HASH),
                ..Default::default()
            }),
            ..Default::default()
        };
        parent = block.header.inner.hash_slow();
        blocks.push(block);
    }
    let header = &mut ledger.block.header.inner;
    header.parent_hash = parent;
    header.beneficiary = REWARDS_ADDRESS;
    header.gas_limit = protocol_block_gas_limit(header.number);
    header.base_fee_per_gas = Some(MIN_PROTOCOL_BASE_FEE);
    header.withdrawals_root = Some(alloy_trie::EMPTY_ROOT_HASH);
    header.extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        execution_summary: Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        }),
        ..Default::default()
    })
    .unwrap();
    ledger.block.body.withdrawals = Some(Default::default());
    ledger.tip.hash = header.hash_slow();
    blocks.push(ledger.block.clone());
    blocks
}

pub(super) struct Successor<'a> {
    pub(super) parent: &'a SealedHeader<OutbeHeader>,
    pub(super) timestamp: u64,
    pub(super) user: Option<Recovered<TransactionSigned>>,
}

pub(super) fn build_successor(
    config: &OutbeEvmConfig,
    provider: &impl StateProviderFactory,
    committee: (&Committee, &CommitteeSnapshot),
    proposer: Address,
    Successor {
        parent,
        timestamp,
        user,
    }: Successor<'_>,
) -> OutbeBlock {
    let (committee, snapshot) = committee;
    let height = parent.header().inner.number;
    let hash = parent.hash();
    let metadata = CertifiedParentAccountingMetadata {
        finalized_block_number: height,
        finalized_block_hash: hash,
        finalized_epoch: 0,
        finalized_view: height + 1,
        parent_view: height,
        ordered_committee: snapshot
            .committee
            .iter()
            .map(|entry| entry.address)
            .collect(),
        signer_bitmap: vec![1; snapshot.committee.len()],
        committee_set_hash: snapshot.committee_set_hash_v2(0),
        vrf_material_version: snapshot.vrf_material_version,
        vrf_group_public_key_hash: alloy_primitives::keccak256(
            &snapshot.vrf_group_public_key_bytes,
        ),
        proof: committee.certify(hash.into(), height).into(),
        ..Default::default()
    };
    let verified =
        outbe_consensus::proof::verify_v2_proof(&metadata, snapshot, &metadata.proof, hash)
            .unwrap();
    let state = provider.state_by_block_hash(hash).unwrap();
    let mut db = StateProviderDatabase::new(EvmStateProviderAdapter(&state));
    let gas_limit = protocol_block_gas_limit(height + 1);
    let extra = Bytes::new();
    let phase1 = config
        .build_signed_phase1_tx(
            height + 1,
            TEST_CHAIN_ID,
            hash,
            Some(metadata.clone()),
            Some(proposer),
        )
        .unwrap();
    let begin = config
        .build_begin_system_txs(
            height + 1,
            TEST_CHAIN_ID,
            gas_limit,
            hash,
            &extra,
            Some(metadata.clone()),
            Some(proposer),
            phase1.clone(),
            None,
        )
        .unwrap();
    let attrs = OutbeNextBlockEnvAttributes {
        inner: NextBlockEnvAttributes {
            timestamp,
            suggested_fee_recipient: REWARDS_ADDRESS,
            prev_randao: verified.prev_randao,
            gas_limit,
            parent_beacon_block_root: None,
            withdrawals: Some(Default::default()),
            extra_data: extra,
            slot_number: None,
        },
        timestamp_millis_part: 0,
        parent_consensus_metadata: Some(metadata),
        proposer_evm_address: Some(proposer),
        execute_outbe_block_hooks: true,
        prebuilt_phase1_tx: phase1,
        parent_artifact_hint: None,
        pending_tee_bootstrap: None,
        execution_read_budget: None,
    };
    let mut builder = config
        .builder_for_next_block(&mut db, parent, attrs)
        .unwrap();
    builder.apply_pre_execution_changes().unwrap();
    for tx in begin.into_iter().chain(user) {
        builder
            .execute_transaction(tx)
            .unwrap_or_else(|error| panic!("transaction at height {}: {error:?}", height + 1));
    }
    builder.executor_mut().apply_end_block_phase().unwrap();
    builder
        .finish(state.as_ref(), None)
        .unwrap()
        .block
        .into_block()
}
