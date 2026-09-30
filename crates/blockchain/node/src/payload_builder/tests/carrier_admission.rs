//! Builder policy for result-vote carriers: a carrier the shared verifier
//! proves invalid is skipped, one whose due window is not closed yet is
//! deferred without blame, a check that cannot read healthy state ends the
//! build, and every other transaction goes straight to execution.

use crate::payload_builder::carrier_admission::{
    admit, decide, CarrierAdmissionAbort, CarrierBlock, CarrierDecision, DeferredResultVoteCarrier,
    InvalidResultVoteCarrier,
};
use alloy_consensus::{SignableTransaction as _, TxEip1559};
use alloy_primitives::{Address, Bytes, Signature, TxKind, B256, U256};
use outbe_metadosis::api::ResultVoteCarrierAdmission;
use outbe_ocomp_protocol::{
    abi::{
        MATERIALIZE_CERTIFIED_NODS_SELECTOR, METADOSIS_ADDRESS, NOD_FACTORY_ADDRESS,
        SUBMIT_LYSIS_RESULT_SELECTOR,
    },
    encode_envelope,
    profile::poc_schema_limits,
    registry::ObjectKind,
    system_carrier::{MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS, OCOMP_SYSTEM_CARRIER_GAS_LIMIT},
};
use outbe_primitives::{error::PrecompileError, OutbeTxEnvelope};
use revm::{
    database_interface::DBErrorMarker,
    primitives::{AddressMap, StorageKey, StorageValue},
    state::{Account, AccountInfo, Bytecode},
    Database, DatabaseCommit,
};

/// State backend that fails every read, standing in for a node whose storage
/// cannot serve the block it is building.
#[derive(Debug)]
struct UnreadableState;

#[derive(Debug, thiserror::Error)]
#[error("state backend unavailable")]
struct Unreadable;

impl DBErrorMarker for Unreadable {}

impl Database for UnreadableState {
    type Error = Unreadable;

    fn basic(&mut self, _address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        Err(Unreadable)
    }

    fn code_by_hash(&mut self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
        Err(Unreadable)
    }

    fn storage(
        &mut self,
        _address: Address,
        _index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        Err(Unreadable)
    }

    fn block_hash(&mut self, _number: u64) -> Result<B256, Self::Error> {
        Err(Unreadable)
    }
}

impl DatabaseCommit for UnreadableState {
    fn commit(&mut self, _changes: AddressMap<Account>) {}
}

fn block() -> CarrierBlock {
    CarrierBlock {
        number: 2,
        timestamp: 1_700_000_000,
        chain_id: 1,
        genesis_hash: B256::repeat_byte(0x01),
        beneficiary: Address::repeat_byte(0x02),
    }
}

fn signed(to: Address, gas_limit: u64, max_fee: u128, input: Vec<u8>) -> OutbeTxEnvelope {
    TxEip1559 {
        chain_id: 1,
        nonce: 0,
        gas_limit,
        max_fee_per_gas: max_fee,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(to),
        value: U256::ZERO,
        input: Bytes::from(input),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

/// A carrier that passes the stateless envelope classification (canonical
/// envelope and result-vote prefix) but whose full vote cannot be decoded,
/// because everything after the prefix is zeroed.
fn prefix_valid_carrier() -> OutbeTxEnvelope {
    let mut body = Vec::new();
    body.extend_from_slice(&[0x31; 32]); // protocol bundle hash
    body.extend_from_slice(&[0x32; 32]); // job id
    body.extend_from_slice(&3u32.to_be_bytes()); // attempt
    body.extend_from_slice(&7u64.to_be_bytes()); // result validator-set epoch
    body.extend_from_slice(&[0x33; 32]); // result committee-set hash
    body.extend_from_slice(&[0x34; 32]); // result OCOMP binding hash
    body.extend_from_slice(&[0x35; 32]); // OCOMP key hash
    body.extend_from_slice(&1u64.to_be_bytes()); // key epoch
    body.resize(180, 0);
    let payload = encode_envelope(ObjectKind::ResultVoteV1, &body, poc_schema_limits().codec)
        .expect("canonical result-vote prefix encodes");

    let padded_len = (payload.len() + 31) & !31;
    let mut input = vec![0_u8; 68 + padded_len];
    input[..4].copy_from_slice(&SUBMIT_LYSIS_RESULT_SELECTOR);
    input[4..36].copy_from_slice(&U256::from(32).to_be_bytes::<32>());
    input[36..68].copy_from_slice(&U256::from(payload.len()).to_be_bytes::<32>());
    input[68..68 + payload.len()].copy_from_slice(&payload);
    signed(
        METADOSIS_ADDRESS,
        OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
        MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS,
        input,
    )
}

#[test]
fn ordinary_transaction_executes_without_reading_state() {
    let transfer = signed(
        Address::repeat_byte(0x11),
        21_000,
        1_000_000_000,
        Vec::new(),
    );

    // Any state read would fail and turn into an abort.
    let decision = admit(&mut UnreadableState, block(), &transfer, Address::ZERO);

    assert!(matches!(decision, CarrierDecision::Execute), "{decision:?}");
}

#[test]
fn nod_materialization_carrier_keeps_its_execution_path() {
    // The pre-check only covers result votes. A NOD materialization carrier,
    // well-formed or not, goes to execution exactly as before, and the
    // unreadable backend proves that no state was read for it.
    let mut input = MATERIALIZE_CERTIFIED_NODS_SELECTOR.to_vec();
    input.resize(68, 0);
    let carrier = signed(
        NOD_FACTORY_ADDRESS,
        OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
        MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS,
        input,
    );

    let decision = admit(
        &mut UnreadableState,
        block(),
        &carrier,
        Address::repeat_byte(0x03),
    );

    assert!(matches!(decision, CarrierDecision::Execute), "{decision:?}");
}

#[test]
fn undecodable_full_vote_is_skipped_before_any_state_read() {
    // The full vote is rejected by stateless decoding, which gives the same
    // answer on every node, so the unreadable backend is never consulted.
    let decision = admit(
        &mut UnreadableState,
        block(),
        &prefix_valid_carrier(),
        Address::repeat_byte(0x03),
    );

    assert!(matches!(decision, CarrierDecision::Skip), "{decision:?}");
}

#[test]
fn only_a_proven_invalid_carrier_is_skipped_and_a_due_window_is_deferred() {
    let validator = Address::repeat_byte(0x04);
    let storage_error = || PrecompileError::Storage("backend".into());

    assert!(matches!(
        decide(ResultVoteCarrierAdmission::Valid {
            represented_validator: validator
        }),
        CarrierDecision::Execute
    ));
    assert!(matches!(
        decide(ResultVoteCarrierAdmission::DeadlinePassed {
            represented_validator: validator
        }),
        CarrierDecision::Execute
    ));
    assert!(matches!(
        decide(ResultVoteCarrierAdmission::InvalidCarrier {
            reason: "bad inner signature".into()
        }),
        CarrierDecision::Skip
    ));
    assert!(matches!(
        decide(ResultVoteCarrierAdmission::DeadlineDueUnclosed { deadline_height: 7 }),
        CarrierDecision::Defer
    ));
    assert!(matches!(
        decide(ResultVoteCarrierAdmission::NotYetOpen { open_height: 9 }),
        CarrierDecision::Abort(CarrierAdmissionAbort::NotYetOpen { open_height: 9 })
    ));
    assert!(matches!(
        decide(ResultVoteCarrierAdmission::StateUnavailable {
            source: storage_error()
        }),
        CarrierDecision::Abort(CarrierAdmissionAbort::StateUnavailable(_))
    ));
    assert!(matches!(
        decide(ResultVoteCarrierAdmission::CorruptCommittedState {
            source: storage_error()
        }),
        CarrierDecision::Abort(CarrierAdmissionAbort::CorruptCommittedState(_))
    ));
}

#[test]
fn only_a_proven_invalid_carrier_is_classified_as_bad() {
    use reth_transaction_pool::error::PoolTransactionError as _;

    assert!(InvalidResultVoteCarrier.is_bad_transaction());
    assert!(!DeferredResultVoteCarrier.is_bad_transaction());
}
