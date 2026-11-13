use super::super::value_policy::{
    classify_boundary_value, BoundaryValue::Credited, BoundaryValue::Rejected,
};
use crate::precompiles::routes::{self, ValuePolicy};
use alloy_primitives::{address, Address, Bytes, B256, U256};
use outbe_primitives::{
    addresses::is_reserved_dynamic_precompile_address,
    block::{BlockHeightRegime, BlockRuntimeContext},
    committee_snapshot::CommitteeSnapshot,
    consensus::{DkgBoundaryArtifact, GenesisValidators},
    consensus_metadata::CertifiedParentAccountingMetadata,
    error::Result,
    reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit},
    storage::{MutationEntitlements, StorageHandle},
    system_tx_context::SystemTxExecutionContext,
};
use revm::interpreter::CallValue;

const PAYABLE_ROUTE_A: Address = address!("0x000000000000000000000000000000000000EE81");
const PAYABLE_ROUTE_B: Address = address!("0x000000000000000000000000000000000000EE82");
const REJECT_ROUTE_A: Address = address!("0x000000000000000000000000000000000000EE83");
const REJECT_ROUTE_B: Address = address!("0x000000000000000000000000000000000000EE84");

#[derive(Debug)]
struct TestValuePolicyHost {
    stub: std::sync::Arc<dyn crate::EvmRuntimeHost>,
}

impl TestValuePolicyHost {
    fn new() -> Self {
        Self {
            stub: crate::host::stub_host(),
        }
    }
}

impl crate::EvmRuntimeHost for TestValuePolicyHost {
    fn block_order(&self) -> crate::system_tx::BlockOrder {
        self.stub.block_order()
    }

    fn marker_addresses(&self) -> &'static [Address] {
        self.stub.marker_addresses()
    }

    fn is_registered_precompile(&self, address: &Address) -> bool {
        matches!(
            *address,
            PAYABLE_ROUTE_A | PAYABLE_ROUTE_B | REJECT_ROUTE_A | REJECT_ROUTE_B
        ) || is_reserved_dynamic_precompile_address(*address)
    }

    fn precompile_is_payable(&self, address: &Address) -> bool {
        matches!(*address, PAYABLE_ROUTE_A | PAYABLE_ROUTE_B)
            || is_reserved_dynamic_precompile_address(*address)
    }

    fn precompile_base_gas(&self, address: &Address, input: &[u8]) -> u64 {
        self.stub.precompile_base_gas(address, input)
    }

    fn dispatch_precompile(
        &self,
        storage: StorageHandle<'_>,
        address: &Address,
        data: &[u8],
        caller: Address,
        value: U256,
    ) -> Result<Bytes> {
        self.stub
            .dispatch_precompile(storage, address, data, caller, value)
    }

    fn dispatch_system_tx(
        &self,
        storage: StorageHandle<'_>,
        data: &[u8],
        caller: Address,
        value: U256,
        tee: &crate::tee_attestation_activation::TeeAttestationChainSpecStateV1,
    ) -> Result<Bytes> {
        self.stub
            .dispatch_system_tx(storage, data, caller, value, tee)
    }

    fn validate_finalized_metadata(
        &self,
        storage: StorageHandle<'_>,
        metadata: &CertifiedParentAccountingMetadata,
    ) -> Result<()> {
        self.stub.validate_finalized_metadata(storage, metadata)
    }

    fn run_finalization_and_slashing(
        &self,
        ctx: &BlockRuntimeContext<'_>,
        metadata: &CertifiedParentAccountingMetadata,
        execution: &SystemTxExecutionContext,
    ) -> Result<()> {
        self.stub
            .run_finalization_and_slashing(ctx, metadata, execution)
    }

    fn authenticate_late_credit(
        &self,
        storage: &StorageHandle<'_>,
        credit: &PerBlockCredit,
    ) -> Result<()> {
        self.stub.authenticate_late_credit(storage, credit)
    }

    fn run_late_finalize_credits(
        &self,
        ctx: &BlockRuntimeContext<'_>,
        artifact: &LateFinalizeCreditsArtifact,
        execution: Option<&SystemTxExecutionContext>,
    ) -> Result<()> {
        self.stub
            .run_late_finalize_credits(ctx, artifact, execution)
    }

    fn apply_boundary_outcome(
        &self,
        storage: StorageHandle<'_>,
        boundary: &DkgBoundaryArtifact,
        block_number: u64,
        timestamp: u64,
    ) -> Result<()> {
        self.stub
            .apply_boundary_outcome(storage, boundary, block_number, timestamp)
    }

    fn prepare_boundary(
        &self,
        storage: StorageHandle<'_>,
        boundary: &DkgBoundaryArtifact,
        block_number: u64,
    ) -> Result<()> {
        self.stub.prepare_boundary(storage, boundary, block_number)
    }

    fn consensus_validators(&self, storage: StorageHandle<'_>) -> Result<Vec<Address>> {
        self.stub.consensus_validators(storage)
    }

    fn is_consensus_participant(
        &self,
        storage: StorageHandle<'_>,
        proposer: Address,
    ) -> Result<bool> {
        self.stub.is_consensus_participant(storage, proposer)
    }

    fn is_validator(&self, storage: StorageHandle<'_>, proposer: Address) -> Result<bool> {
        self.stub.is_validator(storage, proposer)
    }

    fn read_committee_snapshot(
        &self,
        storage: StorageHandle<'_>,
        key: B256,
    ) -> Result<Option<CommitteeSnapshot>> {
        self.stub.read_committee_snapshot(storage, key)
    }

    fn validate_genesis_state_for_regime(
        &self,
        regime: BlockHeightRegime,
        storage: StorageHandle<'_>,
        genesis_validators: Option<&GenesisValidators>,
    ) -> Result<()> {
        self.stub
            .validate_genesis_state_for_regime(regime, storage, genesis_validators)
    }

    fn validate_genesis_state(
        &self,
        storage: StorageHandle<'_>,
        genesis: &GenesisValidators,
    ) -> Result<()> {
        self.stub.validate_genesis_state(storage, genesis)
    }

    fn proposer_attestation(
        &self,
        storage: StorageHandle<'_>,
        proposer: Address,
    ) -> Result<Option<[u8; 32]>> {
        self.stub.proposer_attestation(storage, proposer)
    }

    fn mutation_entitlements(
        &self,
        _call: crate::precompiles::PrecompileMutationCall<'_>,
    ) -> MutationEntitlements {
        MutationEntitlements::NONE
    }

    fn is_protocol_cycle_call(&self, address: Address, caller: Address, data: &[u8]) -> bool {
        self.stub.is_protocol_cycle_call(address, caller, data)
    }

    fn derive_active_utc_day(
        &self,
        storage: StorageHandle<'_>,
        genesis_timestamp: u64,
        block_timestamp: u64,
    ) -> Result<u32> {
        self.stub
            .derive_active_utc_day(storage, genesis_timestamp, block_timestamp)
    }

    fn settle_block_burned_fees(&self, storage: StorageHandle<'_>, amount: U256) -> Result<()> {
        self.stub.settle_block_burned_fees(storage, amount)
    }

    fn user_fee_policy(&self) -> &dyn crate::UserTxFeePolicy {
        self.stub.user_fee_policy()
    }
}

fn policy(address: Address) -> ValuePolicy {
    let host = TestValuePolicyHost::new();
    routes::resolve(&host, &address)
        .expect("address must be a routed precompile")
        .value_policy(&host)
}

/// Apparent value names an amount that was never transferred. Delegated
/// frames are refused before this point, so the arm is defensive.
#[test]
fn apparent_value_is_never_credited() {
    for address in [PAYABLE_ROUTE_A, PAYABLE_ROUTE_B] {
        assert_eq!(
            classify_boundary_value(policy(address), &CallValue::Apparent(U256::from(7u64))),
            Rejected("outbe precompile: apparent value is not a transfer"),
        );
    }
}

#[test]
fn zero_value_dispatches_whatever_the_policy() {
    for address in [REJECT_ROUTE_A, REJECT_ROUTE_B, PAYABLE_ROUTE_A] {
        for value in [
            CallValue::Transfer(U256::ZERO),
            CallValue::Apparent(U256::ZERO),
        ] {
            assert_eq!(
                classify_boundary_value(policy(address), &value),
                Credited(U256::ZERO),
            );
        }
    }
}

#[test]
fn transferred_value_is_credited_to_payable_routes() {
    let amount = U256::from(5u64);
    for address in [PAYABLE_ROUTE_A, PAYABLE_ROUTE_B] {
        assert_eq!(
            classify_boundary_value(policy(address), &CallValue::Transfer(amount)),
            Credited(amount),
        );
    }
}

#[test]
fn transferred_value_to_a_reject_route_is_refused() {
    let amount = U256::from(5u64);
    for address in [REJECT_ROUTE_A, REJECT_ROUTE_B] {
        assert_eq!(
            classify_boundary_value(policy(address), &CallValue::Transfer(amount)),
            Rejected("outbe precompile: non-payable address called with value"),
        );
    }
}

/// Reserving the dynamic precompile address class must not make native value
/// unspendable there when the host marks dynamic routes payable.
#[test]
fn reserved_dynamic_class_permits_value_when_host_allows() {
    let dynamic_account: Address = "0x53c0000000000000000000000000000000000001"
        .parse()
        .expect("valid dynamic precompile class address");
    let amount = U256::from(5u64);
    assert_eq!(policy(dynamic_account), ValuePolicy::Payable);
    assert_eq!(
        classify_boundary_value(policy(dynamic_account), &CallValue::Transfer(amount)),
        Credited(amount),
    );
}
