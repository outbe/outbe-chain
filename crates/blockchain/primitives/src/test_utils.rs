//! Shared artifact construction for tests and protocol benchmarks.

pub mod sol_interface;

use crate::consensus::{DkgBoundaryArtifact, ReshareResult};
use alloy_primitives::{address, Bytes, B256, U256};

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
        tee_recipient_pubkeys: Vec::new(),
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
