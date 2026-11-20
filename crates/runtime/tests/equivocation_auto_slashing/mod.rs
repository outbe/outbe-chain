use std::sync::Arc;

use alloy_genesis::Genesis;
use alloy_primitives::{Address, B256, U256};
use commonware_codec::{DecodeExt as _, Encode as _};
use commonware_cryptography::{bls12381, Signer as _};
use commonware_utils::ordered::Set;
use outbe_execution::{OutbeFullNode, OutbeNode};
use outbe_primitives::{
    equivocation_evidence::{EquivocationEvidenceKind as Kind, VerifiedEquivocationEvidence},
    signer::OutbeEvmSigner,
    storage::hashmap::HashMapStorageProvider,
    OutbeHeader,
};
use outbe_slashindicator::test_signing;
use outbe_staking::contract::Staking;
use outbe_system_common::addresses::STAKING_ADDRESS;
use outbe_validatorset::{contract::ValidatorSet, CommitteeEntry, CommitteeSnapshot};
use reth_chainspec::{Chain, ChainSpecBuilder};
use reth_node_api::TreeConfig;
use reth_node_builder::{EngineNodeLauncher, Node as _, NodeBuilder, NodeConfig};
use reth_node_core::args::{DiscoveryArgs, NetworkArgs};
use reth_provider::providers::BlockchainProvider;
use reth_tasks::Runtime;

mod execution;
pub use execution::{execute, execute_envelope};

pub const ACCUSED: Address = Address::repeat_byte(0xaa);
pub const STAKE: u64 = 1_000_000;
const OWNER: Address = Address::repeat_byte(0xbb);

fn members(sender: Address) -> Vec<CommitteeEntry> {
    let mut members = vec![
        CommitteeEntry {
            address: sender,
            consensus_pubkey: bls12381::PrivateKey::from_seed(1)
                .public_key()
                .encode()
                .as_ref()
                .try_into()
                .unwrap(),
        },
        CommitteeEntry {
            address: ACCUSED,
            consensus_pubkey: test_signing::keypair(77).unwrap().1.to_bytes(),
        },
    ];
    members.sort_by_key(|member| member.consensus_pubkey);
    members
}

pub fn seed(sender: Address) -> HashMapStorageProvider {
    seed_target(sender, true)
}

pub fn seed_target(sender: Address, active: bool) -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_block_number(1);
    provider.enter(|storage| {
        let mut vs = ValidatorSet::new(storage.clone());
        vs.test_configure_registry(OWNER).unwrap();
        vs.test_seed_authoritative_min_stake(U256::from(1)).unwrap();
        let snapshot = CommitteeSnapshot {
            committee: members(sender),
            vrf_material_version: 1,
            vrf_group_public_key_bytes: vec![0x11; 96],
            vrf_public_polynomial_hash: B256::ZERO,
        };
        for member in &snapshot.committee {
            if member.address == ACCUSED && !active {
                vs.test_register_validator_without_pop(member.address, &member.consensus_pubkey)
                    .unwrap();
            } else {
                vs.test_register_active_validator(
                    member.address,
                    &member.consensus_pubkey,
                    U256::from(STAKE),
                )
                .unwrap();
            }
        }
        vs.test_seed_committee_ring(OWNER, &snapshot).unwrap();
        let staking = Staking::new(storage.clone());
        staking.config_min_stake.write(U256::from(1)).unwrap();
        for member in &snapshot.committee {
            staking
                .stake_amount
                .write(&member.address, U256::from(STAKE))
                .unwrap();
        }
        staking.total_staked.write(U256::from(2 * STAKE)).unwrap();
        storage
            .increase_balance(STAKING_ADDRESS, U256::from(2 * STAKE))
            .unwrap();
    });
    provider
}

pub fn evidence(kind: Kind) -> VerifiedEquivocationEvidence {
    let sender = OutbeEvmSigner::from_secret_bytes([1; 32])
        .unwrap()
        .address();
    let committee =
        Set::from_iter_dedup(members(sender).iter().map(|member| {
            bls12381::PublicKey::decode(member.consensus_pubkey.as_slice()).unwrap()
        }));
    let notarize = outbe_primitives::proof::notarize_namespace(&committee);
    let finalize = outbe_primitives::proof::finalize_namespace(&committee);
    let nullify = outbe_primitives::proof::nullify_namespace(&committee);
    let (first_namespace, second_namespace) = match kind {
        Kind::ConflictingNotarize => (&notarize, &notarize),
        Kind::ConflictingFinalize => (&finalize, &finalize),
        Kind::NullifyFinalize => (&nullify, &finalize),
    };
    let (sk, pk) = test_signing::keypair(77).unwrap();
    let first_payload = if kind == Kind::NullifyFinalize {
        test_signing::nullify_payload(1, 5)
    } else {
        test_signing::proposal(1, 5, 4, [1; 32])
    };
    let first_block = test_signing::signed_evidence(
        &sk,
        &pk,
        first_namespace,
        &first_payload,
        test_signing::POP_DST,
    );
    let second_block = test_signing::signed_evidence(
        &sk,
        &pk,
        second_namespace,
        &test_signing::proposal(1, 5, 4, [2; 32]),
        test_signing::POP_DST,
    );
    VerifiedEquivocationEvidence {
        kind,
        epoch: 1,
        accused_pubkey: pk.to_bytes(),
        evidence_hash:
            outbe_primitives::equivocation_evidence::canonical_equivocation_evidence_hash(
                &first_block,
                &second_block,
            ),
        first_block,
        second_block,
    }
}

pub async fn launch(
    seeded: &HashMapStorageProvider,
    signer: Arc<OutbeEvmSigner>,
) -> eyre::Result<(OutbeFullNode, Runtime, tempfile::TempDir)> {
    launch_configured(seeded, signer, 1, 10).await
}

pub async fn launch_configured(
    seeded: &HashMapStorageProvider,
    signer: Arc<OutbeEvmSigner>,
    base_fee: u64,
    price_bump: u128,
) -> eyre::Result<(OutbeFullNode, Runtime, tempfile::TempDir)> {
    outbe_primitives::proof::init_consensus_chain_id(1)?;
    let mut genesis = Genesis::default();
    genesis.config.chain_id = 1;
    genesis.gas_limit = 30_000_000;
    genesis.base_fee_per_gas = Some(base_fee.into());
    for ((address, slot), value) in &seeded.storage {
        let account = genesis.alloc.entry(*address).or_default();
        account.code = Some(vec![0xef].into());
        account
            .storage
            .get_or_insert_with(Default::default)
            .insert(B256::from(*slot), B256::from(*value));
    }
    genesis.alloc.entry(STAKING_ADDRESS).or_default().balance = U256::from(2 * STAKE);
    let chain = Arc::new(
        ChainSpecBuilder::default()
            .chain(Chain::from_id(1))
            .genesis(genesis)
            .cancun_activated()
            .build()
            .map_header(OutbeHeader::new),
    );
    let runtime = Runtime::test();
    let config = NodeConfig::new(chain)
        .with_network(NetworkArgs {
            discovery: DiscoveryArgs {
                disable_discovery: true,
                ..Default::default()
            },
            ..Default::default()
        })
        .with_unused_ports();
    let execution = OutbeNode {
        shutdown: Default::default(),
        bridge: None,
        evm_signer: Some(signer),
        runtime_host: Some(Arc::new(outbe_runtime::OutbeRuntime::new())),
        pool_policy: Arc::new(outbe_runtime::OutbeRuntime::new()),
        rpc_protection: Default::default(),
    };
    let temp = tempfile::TempDir::new()?;
    let db = reth_ethereum::provider::db::init_db(
        temp.path().join("db"),
        reth_ethereum::provider::db::mdbx::DatabaseArguments::test(),
    )?;
    let config = config.with_txpool(reth_node_core::args::TxPoolArgs {
        price_bump,
        ..Default::default()
    });
    let config = config.with_datadir_args(reth_node_core::args::DatadirArgs {
        datadir: temp.path().to_path_buf().into(),
        ..Default::default()
    });
    let handle = NodeBuilder::new(config)
        .with_database(db)
        .with_launch_context(runtime.clone())
        .with_types_and_provider::<OutbeNode, BlockchainProvider<_>>()
        .with_components(execution.components_builder())
        .with_add_ons(execution.add_ons())
        .launch_with_fn(|builder| {
            let launcher = EngineNodeLauncher::new(
                builder.task_executor().clone(),
                builder.config().datadir(),
                TreeConfig::default().with_cross_block_cache_size(1024 * 1024),
            );
            builder.launch_with(launcher)
        })
        .await?;
    Ok((handle.node, runtime, temp))
}

/// Publishes contract fixture transitions through the real provider's state writer.
pub fn publish(node: &OutbeFullNode, seeded: &HashMapStorageProvider) -> eyre::Result<()> {
    use revm::database::states::{PlainStorageChangeset, StateChangeset};
    let mut accounts = std::collections::BTreeMap::<Address, Vec<(U256, U256)>>::new();
    for (&(address, slot), &value) in &seeded.storage {
        accounts.entry(address).or_default().push((slot, value));
    }
    let storage = accounts
        .into_iter()
        .map(|(address, storage)| PlainStorageChangeset {
            address,
            wipe_storage: false,
            storage,
        })
        .collect();
    commit_changes(
        node,
        StateChangeset {
            storage,
            ..Default::default()
        },
    )
}

pub fn seed_unjailed(sender: Address) -> HashMapStorageProvider {
    let mut seeded = seed(sender);
    seeded.set_block_number(2);
    seeded.enter(|storage| {
        let mut vs = ValidatorSet::new(storage.clone());
        vs.jail_validator(ACCUSED).unwrap();
        let mut encoded = 1u64.to_be_bytes().to_vec();
        encoded.extend_from_slice(sender.as_slice());
        vs.activate_reshared_set(&[sender], alloy_primitives::keccak256(encoded))
            .unwrap();
        Staking::new(storage.clone())
            .unjail_validator(ACCUSED)
            .unwrap();
        assert!(matches!(
            ValidatorSet::new(storage)
                .validator_lifecycle(ACCUSED)
                .unwrap(),
            outbe_validatorset::ValidatorLifecycle::WaitingForReadiness(_)
        ));
    });
    seeded
}

/// Advances the canonical provider fixture and the real pool together.
pub fn advance_fee(node: &OutbeFullNode, base_fee: u64) -> eyre::Result<()> {
    advance_block(node, base_fee, vec![])
}

pub fn advance_block(
    node: &OutbeFullNode,
    base_fee: u64,
    transactions: Vec<outbe_primitives::OutbeTxEnvelope>,
) -> eyre::Result<()> {
    use alloy_consensus::BlockHeader as _;
    use reth_primitives_traits::SignedTransaction as _;
    use reth_primitives_traits::{RecoveredBlock, SealedHeader};
    use reth_provider::{
        BlockNumReader as _, BlockWriter as _, CanonChainTracker as _, DBProvider as _,
        DatabaseProviderFactory as _, HeaderProvider as _,
    };
    use reth_transaction_pool::{BlockInfo, TransactionPoolExt as _};
    let parent = node
        .provider
        .sealed_header(node.provider.best_block_number()?)?
        .unwrap();
    let mut header = parent.header().clone();
    header.inner.parent_hash = parent.hash();
    header.inner.number += 1;
    header.inner.timestamp += 1;
    header.inner.base_fee_per_gas = Some(base_fee);
    header.inner.transactions_root =
        alloy_consensus::proofs::calculate_transaction_root(&transactions);
    let senders = transactions
        .iter()
        .cloned()
        .map(|tx| tx.try_into_recovered().unwrap().signer())
        .collect();
    let block = outbe_primitives::OutbeBlock {
        header: header.clone(),
        body: outbe_primitives::OutbeBlockBody {
            transactions,
            ..Default::default()
        },
    };
    let recovered = RecoveredBlock::new_unhashed(block, senders);
    let writer = node.provider.database_provider_rw()?;
    writer.insert_block(&recovered)?;
    writer.commit()?;
    let sealed = SealedHeader::seal_slow(header);
    node.provider.set_canonical_head(sealed.clone());
    node.pool.set_block_info(BlockInfo {
        last_seen_block_hash: sealed.hash(),
        last_seen_block_number: sealed.number(),
        block_gas_limit: sealed.gas_limit(),
        pending_basefee: base_fee,
        pending_blob_fee: None,
    });
    Ok(())
}

/// Reth can read hashed state directly, so publish both state representations.
pub fn commit_changes(
    node: &OutbeFullNode,
    changes: revm::database::states::StateChangeset,
) -> eyre::Result<()> {
    use alloy_primitives::keccak256;
    use reth_provider::{DBProvider as _, DatabaseProviderFactory as _, StateWriter as _};
    let hashed = reth_trie::HashedPostState::default()
        .with_accounts(
            changes
                .accounts
                .iter()
                .map(|(address, info)| (keccak256(address), info.clone().map(Into::into))),
        )
        .with_storages(changes.storage.iter().map(|storage| {
            (
                keccak256(storage.address),
                reth_trie::HashedStorage::from_iter(
                    storage
                        .storage
                        .iter()
                        .map(|(slot, value)| (keccak256(B256::from(*slot)), *value)),
                ),
            )
        }))
        .into_sorted();
    let writer = node.provider.database_provider_rw()?;
    writer.write_state_changes(changes)?;
    writer.write_hashed_state(&hashed)?;
    writer.commit()?;
    Ok(())
}

pub fn seed_inactive(sender: Address) -> HashMapStorageProvider {
    let mut seeded = seed(sender);
    seeded.set_block_number(2);
    seeded.set_timestamp(U256::from(2));
    seeded.enter(|storage| {
        let mut staking = Staking::new(storage.clone());
        staking.config_unbonding_period.write(1).unwrap();
        staking.unstake(ACCUSED, U256::from(STAKE)).unwrap();
        let mut encoded = 1u64.to_be_bytes().to_vec();
        encoded.extend_from_slice(sender.as_slice());
        ValidatorSet::new(storage)
            .activate_reshared_set(&[sender], alloy_primitives::keccak256(encoded))
            .unwrap();
    });
    seeded.set_timestamp(U256::from(4));
    seeded.enter(|storage| {
        Staking::new(storage.clone())
            .claim_unbonded(ACCUSED)
            .unwrap();
        assert!(matches!(
            ValidatorSet::new(storage)
                .validator_lifecycle(ACCUSED)
                .unwrap(),
            outbe_validatorset::ValidatorLifecycle::Inactive(_)
        ));
    });
    seeded
}
