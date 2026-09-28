//! The pool-selection size estimate must bound the sealed block, including the
//! header artifacts that finalization writes after selection.

use crate::payload_builder::size_budget::{BlockSizeBudget, SizeRejection};
use alloy_consensus::{BlockBody, Header, SignableTransaction as _, TxEip1559};
use alloy_eips::eip4895::{Withdrawal, Withdrawals};
use alloy_primitives::{Address, Bloom, Bytes, Signature, TxKind, B256, B64, U256};
use alloy_rlp::Encodable as _;
use outbe_primitives::{
    consensus::{OUTBE_MAX_BLOCK_SIZE, OUTBE_MAX_EXTRA_DATA_SIZE},
    OutbeBlock, OutbeHeader, OutbeTxEnvelope,
};

/// A header whose every field takes its widest encoding, carrying the largest
/// `extra_data` the protocol accepts.
fn widest_header() -> OutbeHeader {
    OutbeHeader {
        inner: Header {
            parent_hash: B256::repeat_byte(0xff),
            ommers_hash: B256::repeat_byte(0xff),
            beneficiary: Address::repeat_byte(0xff),
            state_root: B256::repeat_byte(0xff),
            transactions_root: B256::repeat_byte(0xff),
            receipts_root: B256::repeat_byte(0xff),
            logs_bloom: Bloom::repeat_byte(0xff),
            difficulty: U256::MAX,
            number: u64::MAX,
            gas_limit: u64::MAX,
            gas_used: u64::MAX,
            timestamp: u64::MAX,
            extra_data: Bytes::from(vec![0xff; OUTBE_MAX_EXTRA_DATA_SIZE]),
            mix_hash: B256::repeat_byte(0xff),
            nonce: B64::repeat_byte(0xff),
            base_fee_per_gas: Some(u64::MAX),
            withdrawals_root: Some(B256::repeat_byte(0xff)),
            blob_gas_used: Some(u64::MAX),
            excess_blob_gas: Some(u64::MAX),
            parent_beacon_block_root: Some(B256::repeat_byte(0xff)),
            requests_hash: Some(B256::repeat_byte(0xff)),
            block_access_list_hash: Some(B256::repeat_byte(0xff)),
            slot_number: Some(u64::MAX),
        },
    }
}

fn transaction(input_len: usize) -> OutbeTxEnvelope {
    TxEip1559 {
        chain_id: u64::MAX,
        nonce: u64::MAX,
        gas_limit: u64::MAX,
        max_fee_per_gas: u128::MAX,
        max_priority_fee_per_gas: u128::MAX,
        to: TxKind::Call(Address::repeat_byte(0xff)),
        value: U256::MAX,
        input: Bytes::from(vec![0xff; input_len]),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

#[test]
fn candidate_is_rejected_when_finalized_extra_data_would_not_fit() {
    // Fits a 1 KiB header allowance exactly, but the finalized header may still
    // carry up to `OUTBE_MAX_EXTRA_DATA_SIZE` bytes of artifacts.
    let candidate = OUTBE_MAX_BLOCK_SIZE - 1024;
    let budget = BlockSizeBudget::new(0, 0, false);

    assert_eq!(
        budget.admit(candidate).map_err(|rejection| rejection.limit),
        Err(OUTBE_MAX_BLOCK_SIZE),
    );
}

#[test]
fn estimate_bounds_the_sealed_block_with_the_widest_header() {
    let transactions: Vec<OutbeTxEnvelope> = [0, 1, 55, 56, 4096, 100_000]
        .into_iter()
        .map(transaction)
        .collect();
    let withdrawals = Withdrawals::new(vec![
        Withdrawal {
            index: u64::MAX,
            validator_index: u64::MAX,
            address: Address::repeat_byte(0xff),
            amount: u64::MAX,
        };
        4
    ]);
    let (last, recorded) = transactions.split_last().expect("fixture has transactions");

    let mut budget = BlockSizeBudget::new(0, withdrawals.length(), false);
    for tx in recorded {
        budget.record(tx.length());
    }
    let estimate = budget
        .estimate(last.length())
        .expect("fixture estimate does not overflow");

    let block = OutbeBlock {
        header: widest_header(),
        body: BlockBody {
            transactions,
            ommers: Vec::new(),
            withdrawals: Some(withdrawals),
        },
    };
    assert!(
        block.length() <= estimate,
        "sealed block {} bytes exceeds the selection estimate {estimate}",
        block.length()
    );
}

#[test]
fn estimate_overflow_rejects_instead_of_wrapping() {
    let mut budget = BlockSizeBudget::new(0, 0, false);
    budget.record(usize::MAX);

    assert_eq!(
        budget.admit(1),
        Err(SizeRejection {
            size: usize::MAX,
            limit: OUTBE_MAX_BLOCK_SIZE,
        }),
    );
}

#[test]
fn transport_cap_is_inclusive_at_the_exact_boundary() {
    let mut budget = BlockSizeBudget::new(0, 0, false);
    budget.record(1000);
    let overhead = budget
        .estimate(0)
        .expect("fixture estimate does not overflow");
    let exact = OUTBE_MAX_BLOCK_SIZE - overhead;

    assert_eq!(budget.admit(exact - 1), Ok(()));
    assert_eq!(budget.admit(exact), Ok(()));
    assert_eq!(
        budget.admit(exact + 1),
        Err(SizeRejection {
            size: OUTBE_MAX_BLOCK_SIZE + 1,
            limit: OUTBE_MAX_BLOCK_SIZE,
        }),
    );
}
