//! Shared artifact construction for tests and protocol benchmarks.

pub mod sol_interface;
pub mod state_proof;

use crate::consensus::{DkgBoundaryArtifact, ReshareResult};
use crate::radicle_publication::{
    PublicationAction, PublicationAttestation, PublicationBatch, PublicationBody,
};
use alloy_primitives::{address, Bytes, FixedBytes, B256, U256};

pub fn sample_system_tx_boundary(block_number: u64) -> DkgBoundaryArtifact {
    DkgBoundaryArtifact {
        epoch: 8,
        dkg_cycle: 2,
        freeze_height: block_number - 2,
        planned_activation_height: block_number,
        target_set_hash: B256::repeat_byte(0x33),
        vrf_material_version: 3,
        vrf_group_public_key: B256::repeat_byte(0x44),
        vrf_group_public_key_bytes: Bytes::from_static(&[0x44u8; 96]),
        committee_set_hash: B256::repeat_byte(0x66),
        is_validator_set_change: true,
        outcome: Bytes::from_static(b"boundary"),
        is_full_dkg: false,
        tee_expired_target_exclusions: Vec::new(),
        tee_expired_target_exclusions_hash: B256::ZERO,
        reshare: ReshareResult {
            new_active_set: vec![address!("0x3333333333333333333333333333333333333333")],
            active_set_hash: B256::repeat_byte(0x55),
        },
    }
}

/// Build call-breach test prices for days 100, 99, and earlier.
pub fn call_breach_prices(prices: &[Option<u64>]) -> Vec<(u32, Option<U256>)> {
    prices
        .iter()
        .enumerate()
        .map(|(back, price)| (100 - back as u32, price.map(U256::from)))
        .collect()
}

// Framing fixtures only; these bytes do not prove valid committee signatures.
pub fn sample_radicle_publication_body() -> PublicationBody {
    PublicationBody {
        chain_id: 0x0102030405060708,
        genesis_hash: B256::repeat_byte(1),
        repo_id: FixedBytes::repeat_byte(2),
        descriptor_hash: B256::repeat_byte(3),
        owner_bls_public: FixedBytes::repeat_byte(4),
        players_commitment: B256::repeat_byte(5),
        epoch: 0x1112131415161718,
        material_version: 0x2122232425262728,
        material_hash: B256::repeat_byte(6),
        finalized_anchor_height: 0x3132333435363738,
        finalized_anchor_hash: B256::repeat_byte(7),
        sequence: 0x4142434445464748,
        previous_receipt_hash: B256::repeat_byte(8),
        action: PublicationAction::Review,
        proposal_id: U256::from_be_bytes([12; 32]),
        patch_id: FixedBytes::repeat_byte(13),
        revision_id: FixedBytes::repeat_byte(14),
        expected_target_head: FixedBytes::repeat_byte(15),
        package_manifest_hash: B256::repeat_byte(16),
    }
}

pub fn sample_radicle_publication_batch(count: usize) -> PublicationBatch {
    PublicationBatch {
        body: sample_radicle_publication_body(),
        attestations: (0..count)
            .map(|index| PublicationAttestation {
                participant_index: index as u32,
                signature: FixedBytes::repeat_byte(index as u8 + 1),
            })
            .collect(),
    }
}
