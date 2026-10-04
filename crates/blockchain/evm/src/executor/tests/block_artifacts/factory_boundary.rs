//! StablecoinFactory boundaries (approved, expired, target-execution error) are byte and state equal across proposer and validator execution.

use super::*;

use reth_primitives_traits::Account as TrieAccount;
use reth_trie::test_utils::state_root;
use std::collections::BTreeMap;
const CREATION_BLOCK: u64 = 7;
#[derive(Clone, Copy, Debug)]
pub(super) enum Boundary {
    Approved,
    Expired,
    Error,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Output {
    pub state_root: B256,
    pub receipts_root: B256,
    pub logs_bloom: alloy_primitives::Bloom,
    pub receipt_bytes: Vec<Vec<u8>>,
    pub receipt_success: Vec<bool>,
    pub cumulative_gas: Vec<u64>,
    pub created_logs: usize,
    pub refunded_logs: usize,
    pub burned_logs: usize,
    pub status: ProposalStatus,
    pub settlement: BondSettlement,
    pub factory_count: U256,
    pub registered_token_id: Option<B256>,
    pub token_by_id: Address,
    pub token_by_ticker: Address,
    pub token_code_hash: Option<B256>,
    pub token_total_supply: U256,
    pub issuer_token_balance: U256,
    pub issuer_balance: U256,
    pub vote_balance: U256,
    pub liabilities: U256,
    pub reservation_exists: bool,
}

fn full_state_root(state: &State<CacheDB<EmptyDBTyped<ProviderError>>>) -> B256 {
    let mut accounts: BTreeMap<Address, (AccountInfo, BTreeMap<U256, U256>)> = state
        .database
        .cache
        .accounts
        .iter()
        .filter_map(|(address, account)| {
            account.info().map(|info| {
                (
                    *address,
                    (
                        info,
                        account.storage.iter().map(|(k, v)| (*k, *v)).collect(),
                    ),
                )
            })
        })
        .collect();
    for (address, cached) in &state.cache.accounts {
        match &cached.account {
            Some(current) => {
                let entry = accounts
                    .entry(*address)
                    .or_insert_with(|| (current.info.clone(), BTreeMap::new()));
                entry.0 = current.info.clone();
                entry
                    .1
                    .extend(current.storage.iter().map(|(k, v)| (*k, *v)));
            }
            None => {
                accounts.remove(address);
            }
        }
    }
    state_root(accounts.into_iter().map(|(address, (info, storage))| {
        let bytecode_hash = (!info.code_hash.is_zero() && info.code_hash != keccak256([]))
            .then_some(info.code_hash);
        let account = TrieAccount {
            nonce: info.nonce,
            balance: info.balance,
            bytecode_hash,
        };
        let storage = storage
            .into_iter()
            .filter(|(_, value)| !value.is_zero())
            .map(|(slot, value)| (B256::from(slot.to_be_bytes::<32>()), value));
        (address, (account, storage))
    }))
}

struct FactoryFixture {
    finalization_block: u64,
    proposer: Address,
    issuer: Address,
    validators: [(Address, [u8; 48]); 3],
    expected_token_id: B256,
    expected_token: Address,
}
type PreparedBoundary = (
    State<CacheDB<EmptyDBTyped<ProviderError>>>,
    Arc<OutbeEvmSigner>,
    FactoryFixture,
);
fn prepare_boundary(boundary: Boundary) -> eyre::Result<PreparedBoundary> {
    const CREATION_BLOCK: u64 = 7;
    let finalization_block = CREATION_BLOCK + VOTING_WINDOW_BLOCKS + 1;
    let signer = test_evm_signer();
    let proposer = signer.address();
    let issuer = Address::repeat_byte(0x31);
    let validators = [
        (proposer, dummy_pubkey(0xc1)),
        (Address::repeat_byte(0xc2), dummy_pubkey(0xc2)),
        (Address::repeat_byte(0xc3), dummy_pubkey(0xc3)),
    ];
    let payload = encode_canonical_stablecoin_create(&StablecoinCreatePayload {
        issuer,
        name: "Parity Dollar".into(),
        ticker: "PARUSD".into(),
        iso4217: 840,
        decimals: 6,
        supply_cap: U256::from(1_000_000u64),
        policy_id: U256::from(1u64),
    })?;
    let payload = core::str::from_utf8(&payload)?;

    let mut state =
        state_with_active_validators_seeded_at_block(&validators, CREATION_BLOCK, |_| {});
    let seed_context = BlockContext::new(
        CREATION_BLOCK,
        1_700_000_000,
        CHAIN_ID,
        proposer,
        validators.iter().map(|(address, _)| *address).collect(),
    );
    let (expected_token_id, expected_token) = seed_factory_boundary(
        &mut state,
        seed_context,
        &FactoryProposal {
            boundary,
            issuer,
            payload,
            validators: &validators,
            finalization_block,
        },
    )?;

    Ok((
        state,
        signer,
        FactoryFixture {
            finalization_block,
            proposer,
            issuer,
            validators,
            expected_token_id,
            expected_token,
        },
    ))
}
pub(super) fn run(boundary: Boundary, validator_execution: bool) -> eyre::Result<Output> {
    let (mut state, signer, fixture) = prepare_boundary(boundary)?;
    let FactoryFixture {
        finalization_block,
        proposer,
        ..
    } = fixture;
    let (config, metadata, parent_hash) = execution_config(&fixture, signer);
    let system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: finalization_block,
            parent_hash,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: Some(metadata.clone()),
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    );
    let evm = config.evm_with_env(
        &mut state,
        test_evm_env(finalization_block, REWARDS_ADDRESS),
    );
    let mut execution = execution_ctx(Some(0), Bytes::new());
    execution.inner.parent_hash = parent_hash;
    execution.parent_consensus_metadata = Some(metadata);
    execution.proposer_evm_address = Some(proposer);
    if validator_execution {
        execution.expected_begin_system_txs = system_txs.clone();
    }
    let mut executor = config.create_executor(evm, execution);
    super::with_phase1_verify_disabled(|| executor.apply_pre_execution_changes())?;
    for transaction in system_txs {
        executor.execute_transaction(transaction)?;
    }

    let receipts = executor.receipts().to_vec();
    drop(executor);
    let receipts = observe_receipts(&receipts);

    observe_boundary_state(&mut state, &fixture, boundary, receipts)
}

fn execution_config(
    fixture: &FactoryFixture,
    signer: Arc<OutbeEvmSigner>,
) -> (OutbeEvmConfig, CertifiedParentAccountingMetadata, B256) {
    let FactoryFixture {
        finalization_block,
        validators,
        ..
    } = fixture;
    let finalization_block = *finalization_block;
    let parent_hash = B256::repeat_byte(0x71);
    let mut metadata = test_metadata();
    metadata.finalized_block_number = finalization_block - 1;
    metadata.finalized_block_hash = parent_hash;
    metadata.ordered_committee = validators.iter().map(|(address, _)| *address).collect();
    metadata.signer_bitmap = vec![1; validators.len()];

    let bridge = ConsensusExecutionBridge::new();
    bridge.record_execution_summary_with_state_root(
        metadata.finalized_block_number,
        parent_hash,
        ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        },
        1_700_000_000,
        B256::repeat_byte(0x91),
    );
    let config = OutbeEvmConfig::new_with_bridge(test_chain_spec(), bridge).with_evm_signer(signer);
    (config, metadata, parent_hash)
}
fn observe_boundary_state(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    fixture: &FactoryFixture,
    boundary: Boundary,
    receipts: ReceiptObservation,
) -> eyre::Result<Output> {
    let FactoryFixture {
        finalization_block,
        proposer,
        issuer,
        validators,
        expected_token_id,
        expected_token,
    } = *fixture;
    let contracts = observe_contracts(
        state,
        &FactoryRead {
            finalization_block,
            proposer,
            validators: &validators,
            issuer,
            expected_token,
            expected_token_id,
        },
    )?;
    let token_code_hash = state
        .basic(expected_token)?
        .map(|account| account.code_hash);
    let state_root = full_state_root(state);

    if matches!(boundary, Boundary::Approved) {
        assert_eq!(contracts.registered_token_id, Some(expected_token_id));
    }
    Ok(assemble_output(
        (state_root, token_code_hash),
        receipts,
        contracts,
    ))
}
fn assemble_output(
    roots: (B256, Option<B256>),
    receipts: ReceiptObservation,
    contracts: FactoryContractState,
) -> Output {
    let (state_root, token_code_hash) = roots;
    Output {
        state_root,
        receipts_root: receipts.receipts_root,
        logs_bloom: receipts.block_bloom,
        receipt_bytes: receipts.receipt_bytes,
        receipt_success: receipts.receipt_success,
        cumulative_gas: receipts.cumulative_gas,
        created_logs: receipts.created_logs,
        refunded_logs: receipts.refunded_logs,
        burned_logs: receipts.burned_logs,
        status: contracts.status,
        settlement: contracts.settlement,
        factory_count: contracts.factory_count,
        registered_token_id: contracts.registered_token_id,
        token_by_id: contracts.token_by_id,
        token_by_ticker: contracts.token_by_ticker,
        token_code_hash,
        token_total_supply: contracts.token_total_supply,
        issuer_token_balance: contracts.issuer_token_balance,
        issuer_balance: contracts.issuer_balance,
        vote_balance: contracts.vote_balance,
        liabilities: contracts.liabilities,
        reservation_exists: contracts.reservation_exists,
    }
}
struct FactoryProposal<'a> {
    boundary: Boundary,
    issuer: Address,
    payload: &'a str,
    validators: &'a [(Address, [u8; 48])],
    finalization_block: u64,
}
fn seed_factory_boundary(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    seed_context: BlockContext,
    fixture: &FactoryProposal<'_>,
) -> eyre::Result<(B256, Address)> {
    let FactoryProposal {
        boundary,
        issuer,
        payload,
        validators,
        finalization_block,
    } = *fixture;
    let mut provider = super::DirectStorageProvider::new(state, seed_context.clone());
    let storage = StorageHandle::new(&mut provider);
    storage.set_balance(VOTE_ADDRESS, STABLECOIN_CREATE_BOND)?;
    let predicted =
        StablecoinFactoryContract::new(storage.clone()).predict_token_address(issuer, "PARUSD")?;
    let mut vote = Vote::new(storage.clone());
    let proposal_id = vote.create_proposal_with_value(
        issuer,
        STABLECOIN_FACTORY_ADDRESS,
        payload,
        CREATION_BLOCK,
        STABLECOIN_CREATE_BOND,
        crate::handlers::vote::registry(),
    )?;
    match boundary {
        Boundary::Approved | Boundary::Error => {
            vote.cast_vote_approve(proposal_id, validators[0].0, true, CREATION_BLOCK + 1)?;
            vote.cast_vote_approve(proposal_id, validators[1].0, true, CREATION_BLOCK + 1)?;
        }
        Boundary::Expired => {}
    }
    if matches!(boundary, Boundary::Error) {
        let mut corrupted = vote
            .proposals
            .get(proposal_id)?
            .ok_or_else(|| eyre::eyre!("missing fixture value"))?;
        corrupted.payload = "{".into();
        vote.proposals.update(&corrupted)?;
    }
    let progress_context = BlockRuntimeContext::new(seed_context, storage.clone());
    outbe_accounting::record_phase1_progress(&progress_context, finalization_block - 2)?;
    provider.flush()?;
    Ok(predicted)
}

struct ReceiptObservation {
    receipt_bytes: Vec<Vec<u8>>,
    receipts_root: B256,
    block_bloom: alloy_primitives::Bloom,
    receipt_success: Vec<bool>,
    cumulative_gas: Vec<u64>,
    created_logs: usize,
    refunded_logs: usize,
    burned_logs: usize,
}
fn observe_receipts(receipts: &[Receipt]) -> ReceiptObservation {
    let receipt_bytes = receipts
        .iter()
        .map(|receipt| receipt.with_bloom_ref().encoded_2718())
        .collect();
    let receipt_blooms: Vec<_> = receipts
        .iter()
        .map(|receipt| receipt.with_bloom_ref())
        .collect();
    let receipts_root = alloy_consensus::proofs::calculate_receipt_root(&receipt_blooms);
    let block_bloom = logs_bloom(receipts.iter().flat_map(|receipt| receipt.logs.iter()));
    let receipt_success = receipts.iter().map(|receipt| receipt.success).collect();
    let cumulative_gas = receipts
        .iter()
        .map(|receipt| receipt.cumulative_gas_used)
        .collect();
    let created_logs = receipts
        .iter()
        .flat_map(|receipt| &receipt.logs)
        .filter(|log| {
            log.address == STABLECOIN_FACTORY_ADDRESS
                && log.data.topics().first()
                    == Some(&IStablecoinFactory::StablecoinCreated::SIGNATURE_HASH)
        })
        .count();
    let refunded_logs = receipts
        .iter()
        .flat_map(|receipt| &receipt.logs)
        .filter(|log| {
            log.address == VOTE_ADDRESS
                && log.data.topics().first() == Some(&IVote::ProposalBondRefunded::SIGNATURE_HASH)
        })
        .count();
    let burned_logs = receipts
        .iter()
        .flat_map(|receipt| &receipt.logs)
        .filter(|log| {
            log.address == VOTE_ADDRESS
                && log.data.topics().first() == Some(&IVote::ProposalBondBurned::SIGNATURE_HASH)
        })
        .count();

    ReceiptObservation {
        receipt_bytes,
        receipts_root,
        block_bloom,
        receipt_success,
        cumulative_gas,
        created_logs,
        refunded_logs,
        burned_logs,
    }
}

struct FactoryRead<'a> {
    finalization_block: u64,
    proposer: Address,
    validators: &'a [(Address, [u8; 48])],
    issuer: Address,
    expected_token: Address,
    expected_token_id: B256,
}
struct FactoryContractState {
    status: ProposalStatus,
    settlement: BondSettlement,
    factory_count: U256,
    registered_token_id: Option<B256>,
    token_by_id: Address,
    token_by_ticker: Address,
    token_total_supply: U256,
    issuer_token_balance: U256,
    issuer_balance: U256,
    vote_balance: U256,
    liabilities: U256,
    reservation_exists: bool,
}
fn observe_contracts(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    fixture: &FactoryRead<'_>,
) -> eyre::Result<FactoryContractState> {
    let FactoryRead {
        finalization_block,
        proposer,
        validators,
        issuer,
        expected_token,
        expected_token_id,
    } = *fixture;
    let read_context = BlockContext::new(
        finalization_block,
        1_700_000_000,
        CHAIN_ID,
        proposer,
        validators.iter().map(|(address, _)| *address).collect(),
    );
    let mut provider = super::DirectStorageProvider::new(state, read_context);
    let storage = StorageHandle::new(&mut provider);
    let vote = Vote::new(storage.clone());
    let factory = StablecoinFactoryContract::new(storage.clone());
    let factory_count = factory.token_count()?;
    let (token_total_supply, issuer_token_balance) = if factory_count == U256::ONE {
        let token = StablecoinContract::new(storage.clone(), expected_token);
        (token.total_supply()?, token.balance_of(issuer)?)
    } else {
        (U256::ZERO, U256::ZERO)
    };
    Ok(FactoryContractState {
        status: vote
            .proposals
            .get(U256::from(1u64))?
            .ok_or_else(|| eyre::eyre!("missing fixture value"))?
            .proposal_status()?,
        settlement: vote.proposal_bond(U256::from(1u64))?.settlement,
        factory_count,
        registered_token_id: factory.registered_token_id(expected_token)?,
        token_by_id: factory.token_by_id(expected_token_id)?,
        token_by_ticker: factory.token_by_ticker("PARUSD")?,
        token_total_supply,
        issuer_token_balance,
        issuer_balance: storage.balance(issuer)?,
        vote_balance: storage.balance(VOTE_ADDRESS)?,
        liabilities: vote.bond_liabilities()?,
        reservation_exists: factory.reservations.exists(U256::from(1u64))?,
    })
}

#[test]
fn factory_boundaries_are_byte_equal_across_proposer_and_validator_execution() {
    for boundary in [Boundary::Approved, Boundary::Expired, Boundary::Error] {
        let proposer = run(boundary, false).expect("proposer factory boundary");
        let validator = run(boundary, true).expect("validator factory boundary");
        assert_eq!(
            proposer, validator,
            "{boundary:?} must be byte/state equal across execution roles"
        );
        assert_eq!(proposer.receipt_success, vec![true; 6]);
        assert_eq!(proposer.cumulative_gas.len(), 6);
        match boundary {
            Boundary::Approved => {
                assert_eq!(proposer.created_logs, 1);
                assert_eq!(proposer.refunded_logs, 1);
                assert_eq!(proposer.burned_logs, 0);
                assert_eq!(proposer.status, ProposalStatus::Approved);
                assert_eq!(proposer.settlement, BondSettlement::Refunded);
                assert_eq!(proposer.factory_count, U256::ONE);
                assert_ne!(proposer.token_by_id, Address::ZERO);
                assert_eq!(proposer.token_by_id, proposer.token_by_ticker);
                assert_eq!(
                    proposer.token_code_hash,
                    Some(keccak256(
                        outbe_primitives::addresses::STABLECOIN_MARKER_CODE
                    ))
                );
                assert_eq!(proposer.token_total_supply, U256::ZERO);
                assert_eq!(proposer.issuer_token_balance, U256::ZERO);
                assert_eq!(proposer.issuer_balance, STABLECOIN_CREATE_BOND);
                assert_eq!(proposer.vote_balance, U256::ZERO);
                assert_eq!(proposer.liabilities, U256::ZERO);
                assert!(!proposer.reservation_exists);
            }
            Boundary::Expired => {
                assert_eq!(proposer.created_logs, 0);
                assert_eq!(proposer.refunded_logs, 0);
                assert_eq!(proposer.burned_logs, 1);
                assert_eq!(proposer.status, ProposalStatus::Expired);
                assert_eq!(proposer.settlement, BondSettlement::Burned);
                assert_eq!(proposer.factory_count, U256::ZERO);
                assert_eq!(proposer.issuer_balance, U256::ZERO);
                assert_eq!(proposer.vote_balance, U256::ZERO);
                assert_eq!(proposer.liabilities, U256::ZERO);
                assert!(!proposer.reservation_exists);
                assert!(proposer.registered_token_id.is_none());
                assert_eq!(proposer.token_by_id, Address::ZERO);
                assert_eq!(proposer.token_by_ticker, Address::ZERO);
                assert_eq!(proposer.token_total_supply, U256::ZERO);
            }
            Boundary::Error => {
                assert_eq!(proposer.created_logs, 0);
                assert_eq!(proposer.refunded_logs, 0);
                assert_eq!(proposer.burned_logs, 0);
                assert_eq!(proposer.status, ProposalStatus::Error);
                assert_eq!(proposer.settlement, BondSettlement::Unsettled);
                assert_eq!(proposer.factory_count, U256::ZERO);
                assert_eq!(proposer.issuer_balance, U256::ZERO);
                assert_eq!(proposer.vote_balance, STABLECOIN_CREATE_BOND);
                assert_eq!(proposer.liabilities, STABLECOIN_CREATE_BOND);
                assert!(proposer.reservation_exists);
                assert!(proposer.registered_token_id.is_none());
                assert_eq!(proposer.token_by_id, Address::ZERO);
                assert_eq!(proposer.token_by_ticker, Address::ZERO);
                assert_eq!(proposer.token_total_supply, U256::ZERO);
            }
        }
    }
}
