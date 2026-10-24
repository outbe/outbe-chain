//! The EVM must revert all evidence effects when it rejects the transaction.
//! Invalid reward configuration must leave evidence available for an identical retry.

use alloy_evm::{Evm as _, EvmFactory as _};
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolError, SolEvent};
use blst::min_pk::SecretKey;
use commonware_codec::{varint::UInt, ReadExt as _, Write as _};
use commonware_cryptography::bls12381;
use commonware_utils::ordered::Set;
use outbe_consensus::proof::notarize_namespace;
use outbe_evm::{test_support::seed_storage, OutbeEvmFactory};
use outbe_primitives::{
    addresses::{SLASH_INDICATOR_ADDRESS, STAKING_ADDRESS, VALIDATOR_SET_ADDRESS},
    chain::CHAIN_ID,
    storage::StorageHandle,
};
use outbe_slashindicator::{precompile::ISlashIndicator, schema::SlashIndicator};
use outbe_staking::{contract::Staking, precompile::IStaking};
use outbe_validatorset::{
    contract::ValidatorSet,
    state::{CommitteeEntry, CommitteeSnapshot},
    test_support::StorageOverrides as _,
    ValidatorLifecycle,
};
use reth_ethereum::evm::primitives::EvmEnv;
use revm::{
    context::{result::ExecutionResult, BlockEnv, CfgEnv, TxEnv},
    database::{CacheDB, EmptyDB},
    primitives::{hardfork::SpecId, TxKind},
    state::{AccountInfo, Bytecode},
    Database as _, DatabaseCommit as _,
};

const ACCUSED: Address = Address::new([0xaa; 20]);
const SUBMITTER: Address = Address::new([0xbb; 20]);
const INITIAL_STAKE: u64 = 1_000_000;
const INITIAL_BALANCE: u64 = 1_000_000_000;

fn signed_proposal(sk: &SecretKey, namespace: &[u8], digest: u8) -> Bytes {
    let mut proposal = Vec::new();
    for value in [1u64, 5, 4] {
        UInt(value).write(&mut proposal);
    }
    proposal.extend_from_slice(&[digest; 32]);
    let message = commonware_utils::union_unique(namespace, &proposal);
    let signature = sk.sign(
        &message,
        b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_",
        &[],
    );
    let mut evidence = sk.sk_to_pk().to_bytes().to_vec();
    evidence.extend_from_slice(&signature.to_bytes());
    evidence.extend_from_slice(&proposal);
    evidence.into()
}

fn assert_penalty_state(storage: StorageHandle<'_>, hash: B256, applied: bool) {
    let si = SlashIndicator::new(storage.clone());
    assert_eq!(si.evidence_processed.read(&hash).unwrap(), applied);
    assert_eq!(si.felony_count.read(&ACCUSED).unwrap(), u64::from(applied));
    let expected_stake = U256::from(if applied { 920_000 } else { INITIAL_STAKE });
    let staking = Staking::new(storage.clone());
    assert_eq!(staking.get_stake(ACCUSED).unwrap(), expected_stake);
    assert_eq!(staking.get_total_staked().unwrap(), expected_stake);
    assert_eq!(
        staking.slash_dust_native.read().unwrap(),
        U256::from(if applied { 80_000 } else { 0 })
    );
    let vs = ValidatorSet::new(storage);
    assert_eq!(
        vs.get_validator(ACCUSED).unwrap().unwrap().stake,
        expected_stake
    );
    let lifecycle = vs.validator_lifecycle(ACCUSED).unwrap();
    if applied {
        assert!(matches!(lifecycle, ValidatorLifecycle::JailRetained(_)));
    } else {
        assert!(lifecycle.is_active_status());
    }
}

#[test]
fn invalid_evidence_reward_reverts_transaction_and_identical_signed_evidence_can_retry() {
    let sk = SecretKey::key_gen(&[0x5a; 32], &[]).unwrap();
    let pubkey = sk.sk_to_pk().to_bytes();
    let committee =
        Set::from_iter_dedup([bls12381::PublicKey::read(&mut pubkey.as_slice()).unwrap()]);
    let namespace = notarize_namespace(&committee);
    let first = signed_proposal(&sk, &namespace, 0xa1);
    let second = signed_proposal(&sk, &namespace, 0xb2);
    let (low, high) = if first < second {
        (&first, &second)
    } else {
        (&second, &first)
    };
    let evidence_hash = keccak256([low.as_ref(), high.as_ref()].concat());
    let calldata: Bytes = ISlashIndicator::submitDoubleProposalEvidenceCall {
        block1: first,
        block2: second,
    }
    .abi_encode()
    .into();

    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        SUBMITTER,
        AccountInfo {
            balance: U256::from(INITIAL_BALANCE),
            ..Default::default()
        },
    );
    db.insert_account_info(
        STAKING_ADDRESS,
        AccountInfo {
            balance: U256::from(INITIAL_STAKE),
            ..Default::default()
        },
    );
    // Preserve native-precompile accounts under EIP-161, as genesis does.
    for address in [VALIDATOR_SET_ADDRESS, SLASH_INDICATOR_ADDRESS] {
        let marker = Bytecode::new_legacy([0xef].into());
        db.insert_account_info(
            address,
            AccountInfo {
                code_hash: marker.hash_slow(),
                code: Some(marker),
                ..Default::default()
            },
        );
    }
    seed_storage(&mut db, 100, SUBMITTER, |storage| {
        let mut vs = ValidatorSet::new(storage.clone());
        vs.test_configure_registry(Address::ZERO).unwrap();
        let mut epoch = vs.epoch_snapshot().unwrap();
        epoch.number = U256::from(1);
        vs.test_set_epoch_snapshot(epoch).unwrap();
        vs.test_register_active_validator(ACCUSED, &pubkey, U256::from(INITIAL_STAKE))
            .unwrap();
        let submitter_key = SecretKey::key_gen(&[0x6b; 32], &[])
            .unwrap()
            .sk_to_pk()
            .to_bytes();
        vs.test_register_active_validator(SUBMITTER, &submitter_key, U256::from(1))
            .unwrap();
        vs.test_seed_committee_ring(
            Address::ZERO,
            &CommitteeSnapshot {
                committee: vec![CommitteeEntry {
                    address: ACCUSED,
                    consensus_pubkey: pubkey,
                }],
                vrf_material_version: 1,
                vrf_group_public_key_bytes: vec![0x11; 96],
                vrf_public_polynomial_hash: B256::ZERO,
            },
        )
        .unwrap();
        let staking = Staking::new(storage.clone());
        staking
            .stake_amount
            .write(&ACCUSED, U256::from(INITIAL_STAKE))
            .unwrap();
        staking
            .total_staked
            .write(U256::from(INITIAL_STAKE))
            .unwrap();
        let si = SlashIndicator::new(storage);
        si.config_slash_amount_percent.write(8).unwrap();
        si.config_evidence_reward_percent.write(150).unwrap();
    });

    for nonce in 0..=1 {
        let env = EvmEnv {
            cfg_env: CfgEnv::new()
                .with_chain_id(CHAIN_ID)
                .with_spec_and_mainnet_gas_params(SpecId::PRAGUE),
            block_env: BlockEnv {
                number: U256::from(1),
                timestamp: U256::from(100),
                gas_limit: 30_000_000,
                ..Default::default()
            },
        };
        let outcome = {
            let mut evm = OutbeEvmFactory::default().create_evm(&mut db, env);
            evm.transact_raw(
                TxEnv::builder()
                    .chain_id(Some(CHAIN_ID))
                    .caller(SUBMITTER)
                    .nonce(nonce)
                    .kind(TxKind::Call(SLASH_INDICATOR_ADDRESS))
                    .data(calldata.clone())
                    .gas_limit(5_000_000)
                    .build()
                    .unwrap(),
            )
            .expect("evidence rejection is an EVM outcome")
        };
        if nonce == 0 {
            let ExecutionResult::Revert { output, .. } = &outcome.result else {
                panic!("invalid configuration must revert: {:?}", outcome.result);
            };
            assert_eq!(
                output.as_ref(),
                ISlashIndicator::SlashConfigPercentAbove100 {
                    field: "evidence_reward_percent".into(),
                    value: 150
                }
                .abi_encode()
            );
            assert!(outcome.result.logs().is_empty());
            // Check all storage slots, including the slots that record evidence deduplication.
            for account in outcome.state.values() {
                assert!(account
                    .storage
                    .values()
                    .all(|slot| slot.present_value == slot.original_value));
            }
        } else {
            assert!(outcome.result.is_success(), "{:?}", outcome.result);
            let logs = outcome.result.logs();
            assert_eq!(
                logs.iter().map(|log| log.address).collect::<Vec<_>>(),
                vec![
                    VALIDATOR_SET_ADDRESS,
                    STAKING_ADDRESS,
                    SLASH_INDICATOR_ADDRESS
                ]
            );
            assert_eq!(
                logs[1].data,
                IStaking::Slashed {
                    validator: ACCUSED,
                    bondedSlashed: U256::from(80_000),
                    unbondingSlashed: U256::ZERO,
                }
                .encode_log_data()
            );
            assert_eq!(
                logs.last().unwrap().data,
                ISlashIndicator::EvidenceFelonyApplied {
                    validator: ACCUSED,
                    submitter: SUBMITTER,
                    slashedAmount: U256::from(80_000),
                    submitterReward: U256::from(8_000),
                }
                .encode_log_data()
            );
        }
        // Commit the outcome even when the transaction reverts.
        // This checks the EVM journal result, not an explicit StorageHandle checkpoint.
        db.commit(outcome.state);
        seed_storage(&mut db, 100, SUBMITTER, |storage| {
            assert_penalty_state(storage, evidence_hash, nonce == 1)
        });
        assert_eq!(
            db.basic(STAKING_ADDRESS).unwrap().unwrap().balance,
            // The 80_000 wei slash is below one protocol unit. It stays in the
            // escrow as slash dust, so the escrow balance does not change.
            U256::from(INITIAL_STAKE)
        );
        // Zero gas price isolates the native reward from transaction fees.
        assert_eq!(
            db.basic(SUBMITTER).unwrap().unwrap().balance,
            U256::from(INITIAL_BALANCE + if nonce == 0 { 0 } else { 8_000 })
        );
        if nonce == 0 {
            seed_storage(&mut db, 100, SUBMITTER, |storage| {
                SlashIndicator::new(storage)
                    .config_evidence_reward_percent
                    .write(10)
                    .unwrap();
            });
        }
    }
}
