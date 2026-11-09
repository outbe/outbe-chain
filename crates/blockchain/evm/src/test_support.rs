//! In-memory EVM that drives calls through [`sub_call::run`], for integration tests.

use alloy_evm::eth::EthEvmContext;
use alloy_primitives::{Address, Bytes, Log, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::{
    block::BlockContext,
    chain::CHAIN_ID,
    storage::{
        direct::DirectStorageProvider, StorageHandle, SubCallError, SubCallInput, SubCallOutput,
        SubCallStatus,
    },
};
use revm::{
    context_interface::JournalTr,
    database::{CacheDB, EmptyDB},
    handler::MainContext as _,
    primitives::hardfork::SpecId,
    state::{AccountInfo, Bytecode},
    Context,
};

use crate::sub_call::{self, SubCallEnvironment};

pub type EvmCtx = EthEvmContext<CacheDB<EmptyDB>>;

const CALL_GAS_LIMIT: u64 = 5_000_000;

/// Isolated child-call environment without runtime body readers.
pub fn fresh_environment(self_address: Address, spec: SpecId) -> SubCallEnvironment {
    SubCallEnvironment {
        self_address,
        outer_is_static: false,
        spec,
    }
}

/// Runtime bytecode from a hex fixture.
pub fn runtime_code(hex: &str) -> Bytecode {
    Bytecode::new_raw(Bytes::from(
        alloy_primitives::hex::decode(hex.trim()).expect("hex bytecode fixture"),
    ))
}

pub fn install_call_scan_markers<DB: revm::Database>(db: &mut CacheDB<DB>, code: &Bytecode) {
    for address in [
        outbe_primitives::addresses::NOD_ADDRESS,
        outbe_primitives::addresses::GEM_ADDRESS,
        outbe_primitives::addresses::GEM_FACTORY_ADDRESS,
        outbe_primitives::addresses::INTEX_ADDRESS,
        outbe_primitives::addresses::CREDIS_ADDRESS,
    ] {
        db.insert_account_info(
            address,
            AccountInfo {
                code_hash: code.hash_slow(),
                code: Some(code.clone()),
                ..Default::default()
            },
        );
    }
}

pub fn install_code(db: &mut CacheDB<EmptyDB>, address: Address, code: &Bytecode) {
    db.insert_account_info(
        address,
        AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code.clone()),
            ..Default::default()
        },
    );
}

/// Runs `seed` against `db` as block 1 at `timestamp` and flushes its writes.
pub fn seed_storage<R>(
    db: &mut CacheDB<EmptyDB>,
    timestamp: u64,
    beneficiary: Address,
    seed: impl FnOnce(StorageHandle<'_>) -> R,
) -> R {
    let block = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 1,
        timestamp,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: timestamp.max(1),
        proposer: beneficiary,
        validators: vec![beneficiary],
    };
    let mut provider = DirectStorageProvider::new(db, block);
    let seeded = StorageHandle::enter(&mut provider, seed);
    provider.flush().expect("flush seeded storage");
    seeded
}

pub struct EvmWorld {
    pub ctx: EvmCtx,
}

impl EvmWorld {
    pub fn new(db: CacheDB<EmptyDB>) -> Self {
        Self {
            ctx: Context::mainnet()
                .with_db(db)
                .modify_cfg_chained(|cfg| cfg.chain_id = CHAIN_ID),
        }
    }

    pub fn at(mut self, timestamp: u64) -> Self {
        self.ctx.block.timestamp = U256::from(timestamp);
        self
    }

    pub fn run(
        &mut self,
        caller: Address,
        input: SubCallInput,
    ) -> Result<SubCallOutput, SubCallError> {
        let environment = SubCallEnvironment {
            self_address: caller,
            outer_is_static: false,
            spec: SpecId::PRAGUE,
        };
        sub_call::run(&mut self.ctx, environment, input)
    }

    pub fn call(
        &mut self,
        caller: Address,
        target: Address,
        calldata: Bytes,
        is_static: bool,
    ) -> SubCallOutput {
        self.run(
            caller,
            SubCallInput {
                target,
                value: U256::ZERO,
                calldata,
                gas_limit: CALL_GAS_LIMIT,
                is_static,
            },
        )
        .expect("sub-call must not fail fatally")
    }

    /// A successful call, decoded.
    pub fn ok<C: SolCall>(&mut self, caller: Address, target: Address, call: C) -> C::Return {
        let out = self.call(caller, target, call.abi_encode().into(), false);
        decode_success::<C>(&out)
    }

    /// A successful static call, decoded.
    pub fn view<C: SolCall>(&mut self, caller: Address, target: Address, call: C) -> C::Return {
        let out = self.call(caller, target, call.abi_encode().into(), true);
        decode_success::<C>(&out)
    }

    pub fn logs(&self) -> &[Log] {
        self.ctx.journaled_state.logs()
    }
}

fn decode_success<C: SolCall>(out: &SubCallOutput) -> C::Return {
    assert!(
        matches!(out.status, SubCallStatus::Success),
        "{:?}",
        out.status
    );
    C::abi_decode_returns(&out.returndata).expect("decode call return")
}

#[cfg(feature = "test-utils")]
mod domain_host {
    use alloy_primitives::{Address, Bytes, B256, U256};
    use outbe_primitives::{
        block::{BlockHeightRegime, BlockRuntimeContext},
        committee_snapshot::CommitteeSnapshot,
        consensus::{DkgBoundaryArtifact, GenesisValidators},
        consensus_metadata::CertifiedParentAccountingMetadata,
        error::Result,
        reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit},
        storage::{MetadosisMutationEntitlements, StorageHandle},
        system_tx_context::SystemTxExecutionContext,
        tee_attestation_v1::TeePolicyV1,
        tee_bootstrap_v2::TeeBootstrapV2,
        zero_fee_types::{
            BootstrapAccountView, BootstrapCandidate, BootstrapTransactionView,
            SponsorshipAuthorization, ZeroFeeAuthorization, ZeroFeeCandidate, ZeroFeePolicyError,
            ZeroFeeTransaction,
        },
    };

    use crate::errors::TeeBootstrapHostError;
    use crate::precompiles::MetadosisMutationCall;
    use crate::tee_attestation_activation::TeeAttestationChainSpecStateV1;
    use crate::{EvmRuntimeHost, UserTxFeePolicy};

    #[derive(Debug, Default)]
    pub struct DomainFeePolicy;

    impl UserTxFeePolicy for DomainFeePolicy {
        fn classify(
            &self,
            tx: &ZeroFeeTransaction<'_>,
        ) -> std::result::Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError> {
            outbe_zerofee::registry().classify(tx)
        }

        fn classify_bootstrap(
            &self,
            tx: &BootstrapTransactionView<'_>,
        ) -> Option<BootstrapCandidate> {
            outbe_zerofee::classify_bootstrap(tx)
        }

        fn authorize_bootstrap(
            &self,
            candidate: BootstrapCandidate,
            account: BootstrapAccountView,
        ) -> bool {
            outbe_zerofee::authorize_bootstrap(candidate, account)
        }

        fn authorize_sponsorship(
            &self,
            storage: StorageHandle<'_>,
            signer: Address,
            block_timestamp_secs: u64,
        ) -> std::result::Result<SponsorshipAuthorization, ZeroFeePolicyError> {
            outbe_zerofee::authorize_sponsorship(storage, signer, block_timestamp_secs)
        }

        fn record_sponsorship_use(
            &self,
            storage: StorageHandle<'_>,
            authorization: SponsorshipAuthorization,
        ) -> std::result::Result<u32, ZeroFeePolicyError> {
            outbe_zerofee::record_sponsorship_use(storage, authorization)
        }

        fn sponsored_gas_exceeds_block_budget(&self, used: u64, gas_limit: u64) -> bool {
            outbe_zerofee::sponsored_gas_exceeds_block_budget(used, gas_limit)
        }

        fn authorize_fee_waiver(
            &self,
            storage: StorageHandle<'_>,
            candidate: ZeroFeeCandidate,
        ) -> std::result::Result<ZeroFeeAuthorization, ZeroFeePolicyError> {
            outbe_zerofee::registry().authorize_fee_waiver(storage, candidate)
        }

        fn classify_sponsorship(
            &self,
            tx: &ZeroFeeTransaction<'_>,
        ) -> std::result::Result<(), ZeroFeePolicyError> {
            outbe_zerofee::classify_sponsorship(tx)
        }
    }

    /// Feature host that forwards each call to the extracted domain crate.
    #[derive(Debug, Default)]
    pub struct DomainEvmHost {
        fee: DomainFeePolicy,
    }

    impl DomainEvmHost {
        pub fn new() -> Self {
            Self::default()
        }
    }

    impl EvmRuntimeHost for DomainEvmHost {
        fn exact_addresses(&self) -> &'static [Address] {
            crate::precompiles::routes::EXACT_ADDRESSES
        }

        fn marker_addresses(&self) -> &'static [Address] {
            &crate::executor::marker_addresses::OUTBE_RUNTIME_MARKER_ADDRESSES
        }

        fn precompile_is_payable(&self, address: &Address) -> bool {
            crate::test_support_routes::domain_is_payable(address)
        }

        fn precompile_base_gas(&self, address: &Address, input: &[u8]) -> u64 {
            crate::test_support_routes::domain_base_gas(address, input)
        }

        fn dispatch_precompile(
            &self,
            storage: StorageHandle<'_>,
            address: &Address,
            data: &[u8],
            caller: Address,
            value: U256,
        ) -> Result<Bytes> {
            crate::test_support_routes::dispatch_domain_precompile(
                storage, address, data, caller, value,
            )
        }

        fn dispatch_system_tx(
            &self,
            storage: StorageHandle<'_>,
            data: &[u8],
            caller: Address,
            value: U256,
            tee: &TeeAttestationChainSpecStateV1,
        ) -> Result<Bytes> {
            crate::sys_tx_precompile::dispatch_system_tx_domain(
                storage,
                data,
                caller,
                value,
                crate::sys_tx_precompile::SystemTxRuntime {
                    host: self,
                    tee_attestation_v1: tee,
                },
            )
        }

        fn validate_finalized_metadata(
            &self,
            storage: StorageHandle<'_>,
            metadata: &CertifiedParentAccountingMetadata,
        ) -> Result<()> {
            outbe_rewards::finalization::validate_finalized_metadata(storage, metadata)
        }

        fn run_finalization_and_slashing(
            &self,
            ctx: &BlockRuntimeContext<'_>,
            metadata: &CertifiedParentAccountingMetadata,
            execution: &SystemTxExecutionContext,
        ) -> Result<()> {
            outbe_rewards::finalization::run_finalization_and_slashing(ctx, metadata, execution)
        }

        fn authenticate_late_credit(
            &self,
            storage: &StorageHandle<'_>,
            credit: &PerBlockCredit,
        ) -> Result<()> {
            outbe_rewards::finalization::authenticate_late_credit(storage, credit)
        }

        fn run_late_finalize_credits(
            &self,
            ctx: &BlockRuntimeContext<'_>,
            artifact: &LateFinalizeCreditsArtifact,
            execution: Option<&SystemTxExecutionContext>,
        ) -> Result<()> {
            outbe_rewards::finalization::run_late_finalize_credits(ctx, artifact, execution)
        }

        fn apply_boundary_outcome(
            &self,
            storage: StorageHandle<'_>,
            boundary: &DkgBoundaryArtifact,
            block_number: u64,
            timestamp: u64,
        ) -> Result<()> {
            outbe_validatorset::apply_boundary_outcome(storage, boundary, block_number, timestamp)
        }

        fn prepare_boundary(
            &self,
            storage: StorageHandle<'_>,
            boundary: &DkgBoundaryArtifact,
            block_number: u64,
        ) -> Result<()> {
            outbe_slashindicator::prepare_boundary_epoch_counters(storage, boundary, block_number)
        }

        fn consensus_validators(&self, storage: StorageHandle<'_>) -> Result<Vec<Address>> {
            let mut validators: Vec<Address> =
                outbe_validatorset::contract::ValidatorSet::new(storage)
                    .get_active_consensus_set()?
                    .into_iter()
                    .map(|record| record.validator_address)
                    .collect();
            validators.sort();
            Ok(validators)
        }

        fn is_consensus_participant(
            &self,
            storage: StorageHandle<'_>,
            proposer: Address,
        ) -> Result<bool> {
            outbe_validatorset::is_consensus_participant(storage, proposer)
        }

        fn is_validator(&self, storage: StorageHandle<'_>, proposer: Address) -> Result<bool> {
            outbe_validatorset::is_validator(storage, proposer)
        }

        fn read_committee_snapshot(
            &self,
            storage: StorageHandle<'_>,
            key: B256,
        ) -> Result<Option<CommitteeSnapshot>> {
            outbe_validatorset::state::read_committee_snapshot(storage, key)
        }

        fn validate_genesis_state_for_regime(
            &self,
            regime: BlockHeightRegime,
            storage: StorageHandle<'_>,
            genesis_validators: Option<&GenesisValidators>,
        ) -> Result<()> {
            outbe_staking::genesis::validate_genesis_state_for_regime(
                regime,
                storage,
                genesis_validators,
            )
        }

        fn validate_genesis_state(
            &self,
            storage: StorageHandle<'_>,
            genesis: &GenesisValidators,
        ) -> Result<()> {
            outbe_staking::genesis::validate_genesis_state(storage, genesis)
        }

        fn proposer_attestation(
            &self,
            storage: StorageHandle<'_>,
            proposer: Address,
        ) -> Result<Option<[u8; 32]>> {
            Ok(outbe_teeregistry::TeeRegistry::new(storage)
                .validator_enclave_binding_v1(proposer)?
                .map(|binding| binding.attestation_ed25519.0))
        }

        fn validate_tee_bootstrap_for_build(
            &self,
            payload: &TeeBootstrapV2,
            expected_policy: &TeePolicyV1,
        ) -> std::result::Result<(), TeeBootstrapHostError> {
            match outbe_teeregistry::validate_tee_bootstrap_for_build(payload, expected_policy) {
                Ok(()) => Ok(()),
                Err(
                    outbe_teeregistry::TeeBootstrapBuildError::InvalidBlock1Ost3BootstrapPayload(
                        reason,
                    ),
                ) => Err(TeeBootstrapHostError::InvalidPayload(reason)),
                Err(
                    outbe_teeregistry::TeeBootstrapBuildError::Block1Ost3PolicyScheduleMismatch,
                ) => Err(TeeBootstrapHostError::PolicyScheduleMismatch),
            }
        }

        fn install_initial_tee_policy(
            &self,
            storage: StorageHandle<'_>,
            policy: &TeePolicyV1,
        ) -> Result<()> {
            outbe_teeregistry::TeeRegistry::new(storage).install_initial_policy_v1(policy)
        }

        fn run_tee_bootstrap(
            &self,
            ctx: &BlockRuntimeContext<'_>,
            payload: &TeeBootstrapV2,
            expected_policy: &TeePolicyV1,
        ) -> Result<()> {
            outbe_teeregistry::run_tee_bootstrap_v1(ctx, payload, expected_policy)
        }

        fn is_protocol_cycle_call(&self, address: Address, caller: Address, data: &[u8]) -> bool {
            outbe_cycle::call_authority::is_protocol_cycle_call(address, caller, data)
        }

        fn mutation_entitlements(
            &self,
            call: MetadosisMutationCall<'_>,
        ) -> MetadosisMutationEntitlements {
            outbe_cycle::call_authority::metadosis_mutation_entitlements(
                outbe_cycle::call_authority::MetadosisMutationCall {
                    address: call.address,
                    data: call.data,
                    caller: call.caller,
                    is_static: call.is_static,
                    value: call.value,
                    chain_id: call.chain_id,
                    block_number: call.block_number,
                    timestamp: call.timestamp,
                    cycle_active_utc_day: call.cycle_active_utc_day,
                },
            )
        }

        fn derive_active_utc_day(
            &self,
            storage: StorageHandle<'_>,
            genesis_timestamp: u64,
            block_timestamp: u64,
        ) -> Result<u32> {
            outbe_cycle::handler::derive_active_utc_day(storage, genesis_timestamp, block_timestamp)
        }

        fn settle_block_burned_fees(
            &self,
            mut storage: StorageHandle<'_>,
            amount: U256,
        ) -> Result<()> {
            outbe_promislimit::settle_block_burned_fees(&mut storage, amount)
        }

        fn sync_hyperlane_validators(&self, ctx: &BlockRuntimeContext<'_>) -> Result<()> {
            outbe_hyperlanecontroller::lifecycle::sync_validators(ctx)
        }

        fn enforce_enclave_upgrade_deadline(&self, ctx: &BlockRuntimeContext<'_>) -> Result<()> {
            outbe_slashindicator::enforce_enclave_upgrade_deadline(ctx)
        }

        fn enforce_tee_lease_deadlines(&self, ctx: &BlockRuntimeContext<'_>) -> Result<usize> {
            outbe_slashindicator::enforce_tee_lease_deadlines(ctx)
        }

        fn run_validator_liveness(&self, ctx: &BlockRuntimeContext<'_>) -> Result<()> {
            outbe_slashindicator::run_validator_liveness(ctx)
        }

        fn user_fee_policy(&self) -> &dyn UserTxFeePolicy {
            &self.fee
        }
    }
}

#[cfg(feature = "test-utils")]
pub use domain_host::DomainEvmHost;
