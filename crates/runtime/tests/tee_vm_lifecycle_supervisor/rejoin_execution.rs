//! Small canonical execution ledger for the signed rejoin transactions.
use super::{execution_committee::Committee, proof, Harness, Signer, TEST_CHAIN_ID};
use alloy_consensus::{
    transaction::RlpEcdsaDecodableTx, Header, Receipt, ReceiptEnvelope, TxEip1559, TxLegacy,
};
use alloy_primitives::{Address, Bytes, TxKind, B256, U256};
use alloy_sol_types::{SolCall as _, SolEvent as _};
use evm2::{
    block::bundle_state::{PlainStorageChangeset, StateChangeset},
    env::BlockEnvExt,
    ethereum::TxEnvelope,
    evm::{AccountInfo, CacheDB, EmptyDB},
    SpecId, TxResult,
};
use outbe_evm::{
    factory::OutbeEvmTypes,
    tee_attestation_activation::{TeeAttestationActivationV1, TeeAttestationChainSpecStateV1},
};
use outbe_primitives::{
    addresses::TEE_REGISTRY_ADDRESS,
    block::BlockContext,
    storage::{direct::DirectStorageProvider, StorageHandle},
    tee_attestation_v1::{TeeAttestationManifestV1, TeePolicyScheduleEntryV1, TeePolicyScheduleV1},
};
use outbe_tee::{
    dcap_protocol::DcapOnboardingArtifactV1,
    finalized_admission::{onboarding_registry_slots_v1, CertifiedHeaderV1},
    lifecycle::{LifecycleCheckpointV1, LifecycleOnboardingProofV1},
    registry_view::RenewalBindingV1,
    upgrade_transfer::UpgradeKeyProofV1,
};
use outbe_teeregistry::TeeRegistry;
use outbe_validatorset::contract::ValidatorSet;
use reth_ethereum::evm::EthEvmEnv;
use reth_primitives_traits::{Recovered, SealedBlock, SignedTransaction as _};

const GAS_LIMIT: u64 = 30_000_000;
const BASE_FEE: u64 = 1;

#[derive(Clone)]
pub(super) struct Ledger {
    db: CacheDB<EmptyDB>,
    evm: outbe_evm::OutbeEvmFactory,
    pub(super) tip: LifecycleCheckpointV1,
    pub(super) block: outbe_primitives::OutbeBlock,
}

impl Ledger {
    pub(super) fn new(h: &Harness, public: B256) -> Self {
        let policy = h.chain.policy.clone();
        let schedule = TeePolicyScheduleV1 {
            chain_id: policy.chain_id,
            genesis_hash: policy.genesis_hash,
            entries: vec![TeePolicyScheduleEntryV1 {
                activation_height: 1,
                policy: policy.clone(),
            }],
        };
        let activation = TeeAttestationActivationV1 {
            manifest: TeeAttestationManifestV1 {
                activation_height: 1,
                policy_schedule_hash: schedule.schedule_hash().unwrap(),
                resource_schedule_hash: policy.resource_schedule_hash,
            },
            policy_schedule: schedule,
        };
        let mut ledger = Self {
            db: CacheDB::default(),
            evm: outbe_evm::OutbeEvmFactory::new(std::sync::Arc::new(
                outbe_runtime::OutbeRuntime::new(),
            ))
            .with_genesis_hash(h.chain.at.genesis_hash)
            .with_genesis_timestamp(1)
            .with_tee_attestation_v1(TeeAttestationChainSpecStateV1::Active(activation.into())),
            tip: h.chain.at,
            block: Default::default(),
        };
        ledger.db.insert_account_info(
            &Signer::address(),
            AccountInfo {
                balance: U256::from(10_u64.pow(17)),
                ..Default::default()
            },
        );
        // Preserve native precompile accounts under EIP-161, as genesis does.
        let marker = evm2::bytecode::Bytecode::new_legacy(Bytes::from_static(&[0xef]));
        for address in [
            TEE_REGISTRY_ADDRESS,
            outbe_primitives::addresses::VALIDATOR_SET_ADDRESS,
            outbe_system_common::addresses::STAKING_ADDRESS,
        ] {
            ledger.db.insert_account_info(
                &address,
                AccountInfo {
                    code_hash: marker.hash_slow(),
                    code: Some(marker.clone()),
                    ..Default::default()
                },
            );
        }
        ledger.storage(|state| {
            let mut registry = TeeRegistry::new(state);
            registry.install_initial_policy_v1(&policy).unwrap();
            registry.tribute_offer_public_key.write(public).unwrap();
        });
        ledger.advance(h.chain.at.timestamp, None);
        ledger
    }

    pub(super) fn advance(
        &mut self,
        timestamp: u64,
        tx: Option<&[u8]>,
    ) -> Option<TxResult<OutbeEvmTypes>> {
        let parent = self.tip.hash;
        self.tip.height += 1;
        self.tip.timestamp = timestamp;
        let executed = tx.map(|raw| {
            let signed = TxEip1559::rlp_decode_signed(&mut &raw[1..]).unwrap();
            let evm_tx = Recovered::new_unchecked(
                evm2::ethereum::TxEnvelope::Eip1559(signed.tx().clone()),
                Signer::address(),
            );
            let tx = reth_ethereum::TransactionSigned::from(signed);
            let recovered = tx.clone().try_into_recovered().unwrap();
            assert_eq!(recovered.signer(), Signer::address());
            let detached = {
                let mut evm = self.evm.create_evm(&mut self.db, env(self.tip));
                evm.transact(&evm_tx).unwrap().detach()
            };
            self.db.commit_pending(&detached.pending_state);
            let result = detached.result;
            assert!(result.is_success(), "{result:?}");
            (tx, result)
        });
        let state_root = state_root(&self.db);
        let mut header = Header {
            number: self.tip.height,
            timestamp,
            parent_hash: parent,
            state_root,
            gas_limit: GAS_LIMIT,
            base_fee_per_gas: Some(BASE_FEE),
            ..Default::default()
        };
        if let Some((tx, result)) = &executed {
            header.gas_used = result.tx_gas_used();
            header.transactions_root =
                alloy_consensus::proofs::calculate_transaction_root(std::slice::from_ref(tx));
            let receipt = Receipt {
                status: result.is_success().into(),
                cumulative_gas_used: header.gas_used,
                logs: result.logs.clone(),
            }
            .with_bloom();
            header.logs_bloom = receipt.logs_bloom;
            header.receipts_root =
                alloy_consensus::proofs::calculate_receipt_root(&[ReceiptEnvelope::Eip1559(
                    receipt,
                )]);
        }
        self.tip.state_root = state_root;
        self.tip.hash = header.hash_slow();
        self.block = outbe_primitives::OutbeBlock {
            header: outbe_primitives::OutbeHeader::new(header),
            body: outbe_primitives::OutbeBlockBody {
                transactions: executed.iter().map(|(tx, _)| tx.clone()).collect(),
                ..Default::default()
            },
        };
        executed.map(|(_, result)| result)
    }

    pub(super) fn storage<T>(&mut self, read: impl FnOnce(StorageHandle<'_>) -> T) -> T {
        let block = BlockContext {
            block_number: self.tip.height,
            timestamp: self.tip.timestamp,
            chain_id: TEST_CHAIN_ID,
            genesis_hash: self.tip.genesis_hash,
            genesis_timestamp: 1,
            proposer: Address::ZERO,
            validators: Vec::new(),
            block_gas_limit: GAS_LIMIT,
            user_gas_used: 0,
        };
        let mut provider = DirectStorageProvider::new(&mut self.db, block);
        let result = StorageHandle::enter(&mut provider, read);
        provider.flush().unwrap();
        result
    }

    pub(super) fn snapshot(&self) -> (outbe_primitives::OutbeBlock, StateChangeset) {
        let mut state = StateChangeset::default();
        let mut addresses: std::collections::BTreeSet<Address> =
            self.db.cache.accounts.keys().copied().collect();
        addresses.extend(self.db.cache.storage.keys().copied());
        for address in addresses {
            let storage = self.db.cache.storage.get(&address);
            let info = match self.db.cache.accounts.get(&address) {
                Some(Some(info)) => info.clone(),
                Some(None) => continue,
                None if storage.is_some() => AccountInfo::default(),
                None => continue,
            };
            state.accounts.push((address, Some(info)));
            if let Some(storage) = storage {
                state.storage.push(PlainStorageChangeset {
                    address,
                    wipe_storage: false,
                    storage: storage
                        .slots
                        .iter()
                        .map(|(&slot, &value)| (slot, value))
                        .collect(),
                });
            }
        }
        state.contracts = self
            .db
            .cache
            .contracts
            .iter()
            .map(|(&hash, code)| (hash, code.clone()))
            .collect();
        (self.block.clone(), state)
    }

    pub(super) fn nonce_and_balance(&self) -> (u64, U256) {
        let info = self
            .db
            .account_info(&Signer::address())
            .expect("signer account present");
        (info.nonce, info.balance)
    }

    pub(super) fn binding(&mut self) -> RenewalBindingV1 {
        self.storage(|state| {
            let binding = TeeRegistry::new(state)
                .validator_enclave_binding_v1(Signer::address())
                .unwrap()
                .unwrap();
            RenewalBindingV1::try_from(
                outbe_primitives::tee_registry_abi_v1::NodeEnclaveBindingV1View::from(&binding),
            )
            .unwrap()
        })
    }

    pub(super) fn status(&mut self) -> u8 {
        self.storage(|state| {
            ValidatorSet::new(state)
                .get_validator(Signer::address())
                .unwrap()
                .unwrap()
                .status
        })
    }

    pub(super) fn set_boundary_outcome(&mut self, committee: &Committee) {
        assert_eq!(self.tip.height, 1);
        self.block.header.inner.extra_data =
            super::boundary_artifact::boundary_extra_for(committee.outcome().into()).into();
        self.tip.hash = self.block.header.inner.hash_slow();
    }

    pub(super) fn call(&self, to: Address, data: Bytes) -> Option<Bytes> {
        let mut evm = self.evm.create_evm(self.db.clone(), env(self.tip));
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                chain_id: Some(self.tip.chain_id),
                nonce: self.nonce_and_balance().0,
                gas_price: u128::from(BASE_FEE),
                gas_limit: 500_000,
                to: TxKind::Call(to),
                value: U256::ZERO,
                input: data,
            }),
            Signer::address(),
        );
        let result = evm.transact(&tx).unwrap().discard();
        result.is_success().then_some(result.output)
    }

    pub(super) fn proof(
        &self,
        executed: &TxResult<OutbeEvmTypes>,
        committee: &Committee,
    ) -> LifecycleOnboardingProofV1 {
        use outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1;
        let artifact = executed
            .logs
            .iter()
            .filter(|log| log.address == TEE_REGISTRY_ADDRESS)
            .find_map(|log| {
                ITeeRegistryV1::OfferKeySealedForRegistryV1::decode_log_validate(log).ok()
            })
            .expect("paid register emitted the sealed onboarding artifact")
            .data
            .sealedOfferKey;
        let context = DcapOnboardingArtifactV1::decode_canonical(&artifact)
            .unwrap()
            .context;
        LifecycleOnboardingProofV1 {
            artifact,
            proof: self.opening(committee, onboarding_registry_slots_v1(&context)),
        }
    }

    pub(super) fn opening(&self, committee: &Committee, slots: [B256; 9]) -> UpgradeKeyProofV1 {
        use outbe_consensus::block::ConsensusBlock;
        use outbe_primitives::test_utils::state_proof;
        let account = trie_accounts(&self.db)
            .find_map(|(address, account)| (address == TEE_REGISTRY_ADDRESS).then_some(account))
            .unwrap();
        let (root, account_nodes) =
            state_proof::account_mpt_with_proofs(&trie_accounts(&self.db).collect::<Vec<_>>());
        assert_eq!(root, self.tip.state_root);
        let db_storage = self.db.cache.storage.get(&TEE_REGISTRY_ADDRESS);
        let mut storage_keys: Vec<_> = slots.map(|slot| U256::from_be_bytes(slot.0)).into();
        if let Some(storage) = db_storage {
            storage_keys.extend(
                storage
                    .slots
                    .keys()
                    .filter(|slot| !storage_keys_contains(&slots, **slot))
                    .copied(),
            );
        }
        let storage: Vec<_> = storage_keys
            .iter()
            .map(|slot| {
                (
                    *slot,
                    db_storage
                        .and_then(|s| s.slots.get(slot))
                        .copied()
                        .unwrap_or_default(),
                )
            })
            .collect();
        let (storage_root, storage_nodes) = state_proof::storage_trie(&storage);
        assert_eq!(storage_root, account.storage_root);
        let block = ConsensusBlock::from_sealed(SealedBlock::seal_slow(self.block.clone()));
        assert_eq!(block.block_hash(), self.tip.hash);
        let witness = proof::registry_witness(
            CertifiedHeaderV1 {
                finalization: committee.certify(block.digest(), self.tip.height),
                header: alloy_consensus::private::alloy_rlp::encode(&self.block.header),
                descendants: Vec::new(),
            },
            account,
            &account_nodes[&TEE_REGISTRY_ADDRESS],
            slots
                .into_iter()
                .zip(storage)
                .zip(storage_nodes)
                .map(|((key, (_, value)), nodes)| (key, value, nodes)),
        );
        UpgradeKeyProofV1 {
            anchor_outcome: committee.outcome().into(),
            committee_transitions: vec![],
            admission: witness.encode_canonical().unwrap().into(),
        }
    }

    pub(super) fn seed_validator(&mut self, jailed: bool) {
        self.storage(|storage| {
            let operator = Signer::address();
            let mut vs = ValidatorSet::new(storage);
            vs.config_is_initialized.write(true).unwrap();
            vs.set_config_max_validators(128).unwrap();
            let minimum = U256::from(1_000);
            vs.test_seed_authoritative_min_stake(minimum).unwrap();
            let public = crate::finality::FinalityCommitteeFixture::new(10).public_keys_min_pk()[0];
            vs.register_validator(operator, &public).unwrap();
            vs.test_seed_authoritative_bonded(operator, minimum)
                .unwrap();
            vs.activate_validator_via_boundary_for_test(operator)
                .unwrap();
            if jailed {
                vs.jail_validator_for_tee_expiry(operator).unwrap();
                vs.activate_reshared_set(&[], B256::ZERO).unwrap();
            }
        });
    }
}

pub(super) fn sign_unjail(nonce: u64) -> Bytes {
    use alloy_eips::eip2718::Encodable2718 as _;
    let input = outbe_staking::precompile::IStaking::unjailValidatorCall {}.abi_encode();
    super::test_signers()
        .0
        .sign_eip1559(TxEip1559 {
            chain_id: TEST_CHAIN_ID,
            nonce,
            gas_limit: 500_000,
            max_fee_per_gas: u128::from(BASE_FEE),
            to: TxKind::Call(outbe_system_common::addresses::STAKING_ADDRESS),
            input: input.into(),
            ..Default::default()
        })
        .unwrap()
        .encoded_2718()
        .into()
}

fn env(at: LifecycleCheckpointV1) -> EthEvmEnv<OutbeEvmTypes> {
    EthEvmEnv::new(
        SpecId::PRAGUE,
        BlockEnvExt {
            number: U256::from(at.height),
            timestamp: U256::from(at.timestamp),
            gas_limit: U256::from(GAS_LIMIT),
            basefee: U256::from(BASE_FEE),
            ..Default::default()
        },
        at.chain_id,
    )
}

fn state_root(db: &CacheDB<EmptyDB>) -> B256 {
    alloy_trie::root::state_root_unhashed(trie_accounts(db))
}

fn storage_keys_contains(slots: &[B256], slot: U256) -> bool {
    slots.contains(&B256::from(slot.to_be_bytes::<32>()))
}

fn trie_accounts(
    db: &CacheDB<EmptyDB>,
) -> impl Iterator<Item = (Address, alloy_trie::TrieAccount)> + '_ {
    use alloy_trie::root::storage_root_unhashed;
    let mut addresses: std::collections::BTreeSet<Address> =
        db.cache.accounts.keys().copied().collect();
    addresses.extend(db.cache.storage.keys().copied());
    addresses.into_iter().filter_map(|address| {
        let storage = db.cache.storage.get(&address);
        let info = match db.cache.accounts.get(&address) {
            Some(Some(info)) => info.clone(),
            Some(None) => return None,
            None if storage.is_some() => AccountInfo::default(),
            None => return None,
        };
        let storage_empty = storage.is_none_or(|s| s.slots.values().all(U256::is_zero));
        if info.is_empty() && storage_empty {
            return None;
        }
        let storage_root = storage_root_unhashed(storage.into_iter().flat_map(|s| {
            s.slots.iter().filter_map(|(slot, &value)| {
                (!value.is_zero()).then_some((B256::from(slot.to_be_bytes::<32>()), value))
            })
        }));
        Some((
            address,
            alloy_trie::TrieAccount {
                nonce: info.nonce,
                balance: info.balance,
                code_hash: info.code_hash,
                storage_root,
            },
        ))
    })
}

pub(super) async fn import_register(h: &mut Harness, ledger: &mut Ledger, committee: &Committee) {
    let record = h.journal.load().unwrap().unwrap();
    let raw = record.raw_signed_tx;
    let tx = TxEip1559::rlp_decode_signed(&mut &raw[1..]).unwrap();
    assert!(tx.tx().max_fee_per_gas > 0);
    let (nonce, balance) = ledger.nonce_and_balance();
    let previous = ledger.tip;
    let receipt = ledger.advance(previous.timestamp + 1, Some(&raw)).unwrap();
    assert_eq!(
        ledger.nonce_and_balance(),
        (nonce + 1, balance - U256::from(receipt.tx_gas_used()))
    );
    assert_ne!(ledger.tip.hash, previous.hash);
    assert_ne!(ledger.tip.state_root, previous.state_root);
    h.chain.at = ledger.tip;
    h.chain.binding = Some(ledger.binding());
    h.chain.nonces = [ledger.nonce_and_balance().0; 3];
    h.chain.balance = ledger.nonce_and_balance().1;
    h.chain.proof = Some(ledger.proof(&receipt, committee));
    let step = h.step().await;
    let crate::engine::Step::Enrollment(
        outbe_tee::lifecycle::enrollment::EnrollmentOutcome::Admitted { anchor },
    ) = step
    else {
        panic!("expected admission, got {step:?}");
    };
    assert_eq!(anchor.finalized_height, ledger.tip.height);
    assert_eq!(anchor.finalized_hash, ledger.tip.hash);
    assert!(h.live.offer_key_ready().unwrap());
}
