//! EIP-7702 delegations to native routes execute empty code, not the persisted
//! account marker. All frame accounting and completion remain owned by evm2,
//! with zero account-state mutation.

use crate::factory::OutbeEvmTypes;
use alloy_primitives::Address;
use evm2::{
    bytecode::Bytecode,
    interpreter::{
        instructions as instr,
        private::{Instruction, InstructionFlags},
        InstrStop, InterpreterState, Message, MessageKind, Pc, Result as InstrResult, StackMut,
    },
    EvmFeatures,
};

/// Returns `true` when the call metadata describes an EIP-7702 delegated call
/// whose original target (`call_target`) is NOT a registered native precompile,
/// but whose resolved delegation target (`code_address`) IS a registered native precompile.
fn is_delegated_native_target(
    eip7702: bool,
    host: Option<&dyn crate::EvmRuntimeHost>,
    kind: MessageKind,
    call_target: Address,
    code_address: Address,
) -> bool {
    if !eip7702 {
        return false;
    }
    if !matches!(
        kind,
        MessageKind::Call
            | MessageKind::CallCode
            | MessageKind::DelegateCall
            | MessageKind::StaticCall
    ) {
        return false;
    }
    let Some(host) = host else {
        return false;
    };
    !host.is_registered_precompile(&call_target)
        && code_address != call_target
        && host.is_registered_precompile(&code_address)
}

/// Normalizes a prepared call message whose `call_target` is not a registered
/// precompile, but whose resolved EIP-7702 delegation target (`code_address`) IS
/// a registered precompile, so that it executes empty bytecode instead of the `0xef` marker.
///
/// Used for top-level transaction initial frames (post-authorization) and Rust sub-calls.
pub(crate) fn normalize_delegated_native_call(
    eip7702: bool,
    host: Option<&dyn crate::EvmRuntimeHost>,
    message: &mut Message<OutbeEvmTypes>,
) {
    if is_delegated_native_target(
        eip7702,
        host,
        message.kind,
        message.call_target,
        message.code_address,
    ) {
        message.code = Bytecode::default();
    }
}

/// Safe opcode `0xef` instruction handler for nested EVM calls (`CALL`, `CALLCODE`,
/// `DELEGATECALL`, `STATICCALL`) targeting an EIP-7702 EOA delegated to a registered
/// native precompile carrying the `[0xef]` system marker.
///
/// Invariant: registered Outbe native precompile accounts carry either empty code or the
/// single-byte `[0xef]` EIP-161 persistence marker (never arbitrary user contract bytecode).
/// Checking `pc_offset == 0` ensures only entry-byte execution of that marker completes
/// immediately with `InstrStop::Stop` at 0 gas without mutating state; all other executions
/// of `0xef` (including user bytecode containing `0xef` at any offset) fall through to
/// `instr::invalid`.
pub(crate) struct GuardedNativeMarkerOpcode;

impl Instruction<OutbeEvmTypes> for GuardedNativeMarkerOpcode {
    const FLAGS: InstructionFlags =
        <instr::invalid<OutbeEvmTypes> as Instruction<OutbeEvmTypes>>::FLAGS;

    fn execute(
        pc: &mut Pc,
        stack: StackMut<'_>,
        state: &mut InterpreterState<'_, '_, OutbeEvmTypes>,
    ) -> InstrResult {
        let pc_offset = state.bytecode().pc_offset(*pc);
        let eip7702 = state.feature(EvmFeatures::EIP7702);
        let (kind, call_target, code_address) = {
            let msg = state.message();
            (msg.kind, msg.call_target, msg.code_address)
        };
        if pc_offset == 0
            && is_delegated_native_target(
                eip7702,
                state.host().ext().runtime_host.as_deref(),
                kind,
                call_target,
                code_address,
            )
        {
            return Err(InstrStop::Stop);
        }
        instr::invalid::<OutbeEvmTypes>::execute(pc, stack, state)
    }
}

#[cfg(test)]
mod tests {
    use crate::OutbeEvmFactory;
    use alloy_consensus::{TxEip7702, TxLegacy};
    use alloy_eips::eip7702::{Authorization, SignedAuthorization};
    use alloy_primitives::{keccak256, Address, Bytes, Signature, TxKind, B256, U256};
    use evm2::{
        bytecode::Bytecode,
        env::BlockEnvExt,
        ethereum::{LazyTxEip7702, TxEnvelope},
        evm::{AccountInfo, CacheDB, EmptyDB},
        SpecId,
    };
    use k256::ecdsa::SigningKey;
    use outbe_primitives::{
        block::{BlockHeightRegime, BlockRuntimeContext},
        committee_snapshot::CommitteeSnapshot,
        consensus::{DkgBoundaryArtifact, GenesisValidators},
        consensus_metadata::CertifiedParentAccountingMetadata,
        error::Result,
        reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit},
        storage::{MutationEntitlements, StorageHandle},
        system_tx_context::SystemTxExecutionContext,
    };
    use reth_ethereum::evm::EthEvmEnv;
    use reth_primitives_traits::Recovered;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    const PRECOMPILE: Address = Address::repeat_byte(0x91);
    const CONTRACT_C: Address = Address::repeat_byte(0xc2);
    const NESTED_CALLER: Address = Address::repeat_byte(0xc3);
    const CALLER: Address = Address::repeat_byte(0xa1);
    const MARKER: [u8; 1] = [0xef];
    const CHAIN_ID: u64 = 1;
    const GAS_LIMIT: u64 = 1_000_000;

    #[derive(Debug)]
    struct TestPrecompileHost {
        stub: Arc<dyn crate::EvmRuntimeHost>,
        calls: Arc<AtomicUsize>,
    }

    impl TestPrecompileHost {
        fn new() -> (Arc<Self>, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            let host = Arc::new(Self {
                stub: crate::host::stub_host(),
                calls: Arc::clone(&calls),
            });
            (host, calls)
        }
    }

    impl crate::EvmRuntimeHost for TestPrecompileHost {
        fn block_order(&self) -> crate::system_tx::BlockOrder {
            self.stub.block_order()
        }
        fn marker_addresses(&self) -> &'static [Address] {
            self.stub.marker_addresses()
        }
        fn is_registered_precompile(&self, address: &Address) -> bool {
            *address == PRECOMPILE
        }
        fn precompile_is_payable(&self, _address: &Address) -> bool {
            false
        }
        fn precompile_base_gas(&self, _address: &Address, _input: &[u8]) -> u64 {
            outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS
        }
        fn dispatch_precompile(
            &self,
            _storage: StorageHandle<'_>,
            _address: &Address,
            _data: &[u8],
            _caller: Address,
            _value: U256,
        ) -> Result<Bytes> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Bytes::from_static(b"native-ok"))
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
        fn is_protocol_cycle_call(
            &self,
            _address: Address,
            _caller: Address,
            _data: &[u8],
        ) -> bool {
            false
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

    fn test_env() -> EthEvmEnv<crate::factory::OutbeEvmTypes> {
        EthEvmEnv::new(
            SpecId::PRAGUE,
            BlockEnvExt {
                beneficiary: Address::repeat_byte(0xfe),
                gas_limit: U256::from(30_000_000_u64),
                ..Default::default()
            },
            CHAIN_ID,
        )
    }

    fn put_code(db: &mut CacheDB<EmptyDB>, address: Address, balance: U256, code: Bytecode) {
        let code_hash = code.hash_slow();
        db.insert_account_info(
            &address,
            AccountInfo {
                balance,
                code_hash,
                code: Some(code),
                ..Default::default()
            },
        );
    }

    fn sign_auth(
        key: &SigningKey,
        chain_id: u64,
        delegate: Address,
        nonce: u64,
    ) -> SignedAuthorization {
        let auth = Authorization {
            chain_id: U256::from(chain_id),
            address: delegate,
            nonce,
        };
        let hash = auth.signature_hash();
        let (sig, recid) = key
            .sign_prehash_recoverable(hash.as_slice())
            .expect("sign authorization");
        let signature = Signature::from_signature_and_parity(sig, recid.is_y_odd());
        auth.into_signed(signature)
    }

    fn address_of(key: &SigningKey) -> Address {
        let verifying = key.verifying_key();
        let encoded = verifying.to_encoded_point(false);
        let hash = keccak256(&encoded.as_bytes()[1..]);
        Address::from_slice(&hash[12..])
    }

    /// Contract C: stores EXTCODEHASH(P) at slot 0, EXTCODESIZE(P) at slot 1, then SELFDESTRUCT(P).
    fn probe_and_selfdestruct_code(target: Address) -> Vec<u8> {
        let mut code = Vec::new();
        // PUSH20 target, EXTCODEHASH (0x3f), PUSH1 0x00, SSTORE (0x55)
        code.push(0x73);
        code.extend_from_slice(target.as_slice());
        code.extend_from_slice(&[0x3f, 0x60, 0x00, 0x55]);
        // PUSH20 target, EXTCODESIZE (0x3b), PUSH1 0x01, SSTORE (0x55)
        code.push(0x73);
        code.extend_from_slice(target.as_slice());
        code.extend_from_slice(&[0x3b, 0x60, 0x01, 0x55]);
        // PUSH20 target, SELFDESTRUCT (0xff)
        code.push(0x73);
        code.extend_from_slice(target.as_slice());
        code.push(0xff);
        code
    }

    /// Contract that performs a nested call (`CALL`, `CALLCODE`, `DELEGATECALL`, or `STATICCALL`)
    /// to `target` with 0 value and 0 calldata, then stores the call success flag at slot 0.
    fn nested_call_contract(opcode: u8, target: Address) -> Vec<u8> {
        let mut code = Vec::new();
        // retLen=0, retOff=0, argLen=0, argOff=0
        code.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00]);
        if matches!(opcode, 0xf1 | 0xf2) {
            // value=0 for CALL / CALLCODE
            code.extend_from_slice(&[0x60, 0x00]);
        }
        // addr = target
        code.push(0x73);
        code.extend_from_slice(target.as_slice());
        // gas = GAS
        code.push(0x5a);
        // CALL / CALLCODE / DELEGATECALL / STATICCALL
        code.push(opcode);
        // SSTORE(slot 0, success)
        code.extend_from_slice(&[0x60, 0x00, 0x55]);
        code
    }

    #[test]
    fn invalid_eip7702_auth_to_precompile_cannot_mask_marker_or_bypass_selfdestruct_guard() {
        let (host, calls) = TestPrecompileHost::new();
        let eoa_key = SigningKey::from_bytes(&[0x42; 32].into()).unwrap();
        let eoa = address_of(&eoa_key);
        let initial_balance = U256::from(10_000_u64);

        let mut db = CacheDB::new(EmptyDB::default());
        put_code(
            &mut db,
            PRECOMPILE,
            U256::ZERO,
            Bytecode::new_legacy(MARKER.into()),
        );
        put_code(
            &mut db,
            CONTRACT_C,
            U256::ZERO,
            Bytecode::new_legacy(probe_and_selfdestruct_code(PRECOMPILE).into()),
        );
        put_code(
            &mut db,
            eoa,
            initial_balance,
            Bytecode::new_eip7702(CONTRACT_C),
        );

        // EOA sends a type-4 tx to itself with an INVALID authorization (wrong nonce: 99 != 1)
        // naming PRECOMPILE. Contract C must still observe PRECOMPILE's real [0xef] marker and
        // SELFDESTRUCT(PRECOMPILE) must redirect EOA's balance to Address::ZERO.
        let invalid_auth = sign_auth(&eoa_key, CHAIN_ID, PRECOMPILE, 99);
        let tx = Recovered::new_unchecked(
            TxEnvelope::Eip7702(LazyTxEip7702::from_recovered_authorizations(TxEip7702 {
                chain_id: CHAIN_ID,
                nonce: 0,
                gas_limit: GAS_LIMIT,
                max_fee_per_gas: 0,
                max_priority_fee_per_gas: 0,
                to: eoa,
                value: U256::ZERO,
                access_list: Default::default(),
                authorization_list: vec![invalid_auth],
                input: Bytes::new(),
            })),
            eoa,
        );

        let mut evm = OutbeEvmFactory::new(host).create_evm(db, test_env());
        let detached = evm.transact(&tx).expect("tx executes").detach();
        assert!(detached.result.is_success(), "{:?}", detached.result);
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let eoa_changes = detached
            .pending_state
            .account_changes_for(&eoa)
            .expect("EOA changes recorded");
        assert_eq!(
            eoa_changes.storage.slots[&U256::ZERO].value.current,
            U256::from_be_bytes(keccak256(MARKER).0),
            "EXTCODEHASH(P) must equal keccak256([0xef])"
        );
        assert_eq!(
            eoa_changes.storage.slots[&U256::from(1)].value.current,
            U256::from(1),
            "EXTCODESIZE(P) must equal 1"
        );

        let zero_balance = detached
            .pending_state
            .account_changes_for(&Address::ZERO)
            .and_then(|acc| acc.current.as_ref().map(|info| info.balance))
            .unwrap_or(U256::ZERO);
        assert_eq!(
            zero_balance, initial_balance,
            "SELFDESTRUCT(P) must redirect EOA balance to Address::ZERO"
        );
        let precompile_balance = detached
            .pending_state
            .account_changes_for(&PRECOMPILE)
            .and_then(|acc| acc.current.as_ref().map(|info| info.balance))
            .unwrap_or(U256::ZERO);
        assert_eq!(
            precompile_balance,
            U256::ZERO,
            "zero wei may strand on PRECOMPILE"
        );
    }

    #[test]
    fn valid_same_tx_redelegation_to_precompile_executes_empty_code_without_mutating_marker() {
        let (host, calls) = TestPrecompileHost::new();
        let eoa_key = SigningKey::from_bytes(&[0x43; 32].into()).unwrap();
        let eoa = address_of(&eoa_key);

        let mut db = CacheDB::new(EmptyDB::default());
        put_code(
            &mut db,
            PRECOMPILE,
            U256::ZERO,
            Bytecode::new_legacy(MARKER.into()),
        );
        put_code(
            &mut db,
            CONTRACT_C,
            U256::ZERO,
            Bytecode::new_legacy(probe_and_selfdestruct_code(PRECOMPILE).into()),
        );
        put_code(
            &mut db,
            eoa,
            U256::from(10_000_u64),
            Bytecode::new_eip7702(CONTRACT_C),
        );

        // Sender's tx nonce is 0 (bumped to 1 during prepare), so valid self-authorization uses nonce 1.
        let valid_auth = sign_auth(&eoa_key, CHAIN_ID, PRECOMPILE, 1);
        let tx = Recovered::new_unchecked(
            TxEnvelope::Eip7702(LazyTxEip7702::from_recovered_authorizations(TxEip7702 {
                chain_id: CHAIN_ID,
                nonce: 0,
                gas_limit: GAS_LIMIT,
                max_fee_per_gas: 0,
                max_priority_fee_per_gas: 0,
                to: eoa,
                value: U256::ZERO,
                access_list: Default::default(),
                authorization_list: vec![valid_auth],
                input: Bytes::new(),
            })),
            eoa,
        );

        let mut evm = OutbeEvmFactory::new(host).create_evm(db, test_env());
        let detached = evm.transact(&tx).expect("tx executes").detach();
        assert!(detached.result.is_success(), "{:?}", detached.result);
        assert!(detached.result.output.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        // Contract C did not run and PRECOMPILE's marker code was never mutated.
        let eoa_changes = detached
            .pending_state
            .account_changes_for(&eoa)
            .expect("EOA delegated");
        assert!(eoa_changes.storage.slots.is_empty());
        assert_eq!(
            eoa_changes
                .current
                .as_ref()
                .and_then(|info| info.code.as_ref())
                .and_then(Bytecode::eip7702_address),
            Some(PRECOMPILE)
        );
        assert!(
            detached
                .pending_state
                .account_changes_for(&PRECOMPILE)
                .is_none_or(|acc| acc.current == acc.original),
            "PRECOMPILE account must not be modified in state"
        );
    }

    #[test]
    fn delegated_eoa_to_precompile_executes_empty_code_top_level_and_nested_while_direct_dispatches(
    ) {
        let (host, calls) = TestPrecompileHost::new();
        let delegated_eoa = Address::repeat_byte(0xd5);

        let mut db = CacheDB::new(EmptyDB::default());
        put_code(
            &mut db,
            PRECOMPILE,
            U256::ZERO,
            Bytecode::new_legacy(MARKER.into()),
        );
        put_code(
            &mut db,
            delegated_eoa,
            U256::ZERO,
            Bytecode::new_eip7702(PRECOMPILE),
        );
        db.insert_account_info(
            &CALLER,
            AccountInfo {
                balance: U256::from(1_000_000_u64),
                ..Default::default()
            },
        );

        let mut evm = OutbeEvmFactory::new(host).create_evm(db, test_env());

        // 1. Top-level call to delegated EOA executes empty code (success, empty output, 0 precompile calls).
        let top_tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                chain_id: Some(CHAIN_ID),
                nonce: 0,
                gas_price: 0,
                gas_limit: GAS_LIMIT,
                to: TxKind::Call(delegated_eoa),
                value: U256::ZERO,
                input: Bytes::new(),
            }),
            CALLER,
        );
        let top_res = evm.transact(&top_tx).expect("top-level executes").commit();
        assert!(top_res.is_success(), "{top_res:?}");
        assert!(top_res.output.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // 2. Direct call to PRECOMPILE dispatches the registered Outbe precompile.
        let direct_tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                chain_id: Some(CHAIN_ID),
                nonce: 1,
                gas_price: 0,
                gas_limit: GAS_LIMIT,
                to: TxKind::Call(PRECOMPILE),
                value: U256::ZERO,
                input: Bytes::new(),
            }),
            CALLER,
        );
        let direct_res = evm.transact(&direct_tx).expect("direct executes").commit();
        assert!(direct_res.is_success(), "{direct_res:?}");
        assert_eq!(direct_res.output, Bytes::from_static(b"native-ok"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // 3. Nested CALL, CALLCODE, DELEGATECALL, and STATICCALL to delegated EOA all succeed
        // with empty code (push 1), do not dispatch PRECOMPILE, and do not mutate PRECOMPILE's state.
        for opcode in [0xf1_u8, 0xf2, 0xf4, 0xfa] {
            let (nested_host, nested_calls) = TestPrecompileHost::new();
            let mut nested_db = CacheDB::new(EmptyDB::default());
            put_code(
                &mut nested_db,
                PRECOMPILE,
                U256::ZERO,
                Bytecode::new_legacy(MARKER.into()),
            );
            put_code(
                &mut nested_db,
                delegated_eoa,
                U256::ZERO,
                Bytecode::new_eip7702(PRECOMPILE),
            );
            put_code(
                &mut nested_db,
                NESTED_CALLER,
                U256::ZERO,
                Bytecode::new_legacy(nested_call_contract(opcode, delegated_eoa).into()),
            );
            nested_db.insert_account_info(
                &CALLER,
                AccountInfo {
                    balance: U256::from(1_000_000_u64),
                    ..Default::default()
                },
            );
            let mut nested_evm =
                OutbeEvmFactory::new(nested_host).create_evm(nested_db, test_env());
            let nested_tx = Recovered::new_unchecked(
                TxEnvelope::Legacy(TxLegacy {
                    chain_id: Some(CHAIN_ID),
                    nonce: 0,
                    gas_price: 0,
                    gas_limit: GAS_LIMIT,
                    to: TxKind::Call(NESTED_CALLER),
                    value: U256::ZERO,
                    input: Bytes::new(),
                }),
                CALLER,
            );
            let detached = nested_evm
                .transact(&nested_tx)
                .expect("nested call executes")
                .detach();
            assert!(
                detached.result.is_success(),
                "opcode 0x{opcode:02x}: {:?}",
                detached.result
            );
            assert_eq!(nested_calls.load(Ordering::SeqCst), 0);
            let caller_changes = detached
                .pending_state
                .account_changes_for(&NESTED_CALLER)
                .expect("NESTED_CALLER wrote slot 0");
            assert_eq!(
                caller_changes.storage.slots[&U256::ZERO].value.current,
                U256::from(1),
                "nested opcode 0x{opcode:02x} to delegated EOA must succeed"
            );
            assert!(
                detached
                    .pending_state
                    .account_changes_for(&PRECOMPILE)
                    .is_none_or(|acc| acc.current == acc.original),
                "nested opcode 0x{opcode:02x} must not mutate PRECOMPILE account state"
            );
        }
    }
}
