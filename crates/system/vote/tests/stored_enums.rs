use std::fmt::Debug;

use outbe_primitives::error::PrecompileError;
use outbe_vote::{
    errors::VoteError,
    schema::{BondSettlement, ProposalStatus},
    VoteKind,
};

fn assert_stored_enum<T: Copy + Debug + PartialEq>(
    variants: &[(u8, T)],
    decode: fn(u8) -> Result<T, VoteError>,
    encode: fn(T) -> u8,
    invalid: VoteError,
) {
    let invalid_kind = std::mem::discriminant(&invalid);
    let invalid_message = PrecompileError::from(invalid).to_string();
    for (stored, variant) in variants {
        assert_eq!(encode(*variant), *stored);
    }
    for stored in u8::MIN..=u8::MAX {
        let expected = variants
            .iter()
            .find_map(|(byte, variant)| (*byte == stored).then_some(*variant));
        let actual = decode(stored);
        assert_eq!(actual.as_ref().ok(), expected.as_ref(), "byte {stored}");
        let actual_error = actual.err().map(|error| {
            (
                std::mem::discriminant(&error),
                PrecompileError::from(error).to_string(),
            )
        });
        let expected_error = expected
            .is_none()
            .then(|| (invalid_kind, invalid_message.clone()));
        assert_eq!(actual_error, expected_error, "byte {stored}");
    }
}

#[test]
fn proposal_status_preserves_all_storage_bytes_and_errors() {
    assert_stored_enum(
        &[
            (0, ProposalStatus::Pending),
            (1, ProposalStatus::Approved),
            (2, ProposalStatus::Rejected),
            (3, ProposalStatus::Expired),
            (4, ProposalStatus::Error),
        ],
        ProposalStatus::from_u8,
        ProposalStatus::to_u8,
        VoteError::InvalidProposalStatus,
    );
}

#[test]
fn bond_settlement_preserves_all_storage_bytes_and_errors() {
    assert_stored_enum(
        &[
            (0, BondSettlement::NoBond),
            (1, BondSettlement::Unsettled),
            (2, BondSettlement::Refunded),
            (3, BondSettlement::Burned),
        ],
        BondSettlement::from_u8,
        BondSettlement::to_u8,
        VoteError::InvalidBondSettlement,
    );
}

#[test]
fn vote_kind_preserves_all_storage_bytes_and_errors() {
    assert_stored_enum(
        &[(0, VoteKind::No), (1, VoteKind::Yes)],
        VoteKind::from_u8,
        VoteKind::to_u8,
        VoteError::InvalidVoteKind,
    );
}
