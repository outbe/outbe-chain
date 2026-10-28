//! The profile selector and the lifecycle storage layout.

use super::*;

#[test]
fn config_unset_resolves_by_chain_id() {
    with_registry(|s| {
        let intex = IntexContract::new(s.clone());
        // An unset selector resolves by network, and CHAIN_ID is not mainnet.
        assert_eq!(config::read(&s).unwrap(), config::IntexParams::DEV);
        // An explicit selector still wins over the network default.
        intex.config_profile.write(config::PROFILE_PROD).unwrap();
        assert_eq!(config::read(&s).unwrap(), config::IntexParams::PROD);
        assert_eq!(
            config::IntexParams::PROD.commit_bond_minor,
            100_000_000u128 * 1_000_000u128
        );
        assert_eq!(
            config::IntexParams::DEV.commit_bond_minor,
            100u128 * 1_000_000u128
        );
    });
}

#[test]
fn config_unknown_selector_errors() {
    with_registry(|s| {
        IntexContract::new(s.clone())
            .config_profile
            .write(99u8)
            .unwrap();
        assert!(config::read(&s).is_err());
    });
}

/// The network default: only mainnet runs the real timings.
#[test]
fn config_auto_profile_follows_the_network() {
    use outbe_primitives::chain::{DEVNET_CHAIN_ID, MAINNET_CHAIN_ID, TESTNET_CHAIN_ID};

    assert_eq!(
        config::IntexParams::from_selector(config::PROFILE_AUTO, MAINNET_CHAIN_ID).unwrap(),
        config::IntexParams::PROD
    );
    for chain_id in [TESTNET_CHAIN_ID, DEVNET_CHAIN_ID, 31_337] {
        assert_eq!(
            config::IntexParams::from_selector(config::PROFILE_AUTO, chain_id).unwrap(),
            config::IntexParams::DEV
        );
    }
}

/// PROD keeps 2.28 times the entry price. DEV uses 1.04 times the entry price.
#[test]
fn config_call_rate_is_pinned_per_profile() {
    assert_eq!(
        config::IntexParams::PROD.call_rate,
        crate::constants::CALL_RATE_PCT
    );
    assert_eq!(config::IntexParams::PROD.call_rate, 128);
    assert_eq!(config::IntexParams::DEV.call_rate, 4);
}

/// Slots are dense in `order` sequence, so a field inserted rather than appended moves every
/// one after it. The seeder writes `config_profile` as a raw slot.
#[test]
fn intex_lifecycle_slot_layout_is_pinned() {
    with_registry(|s| {
        let intex = IntexContract::new(s.clone());
        let slots = [
            intex.call_bin_tree_root.base_slot(),
            intex.call_bin_tree_mid.base_slot(),
            intex.call_bin_tree_leaf.base_slot(),
            intex.call_bin_count.base_slot(),
            intex.config_profile.slot(),
            intex.call_currency_cursor.slot(),
            intex.call_bin_cursor.base_slot(),
            intex.call_group_members.base_slot(),
            intex.call_group_position.base_slot(),
            intex.call_group_slot.base_slot(),
            intex.call_sweep_day.slot(),
            intex.call_bin_groups.base_slot(),
            intex.forfeit_tree_root.slot(),
            intex.forfeit_tree_mid.base_slot(),
            intex.forfeit_tree_leaf.base_slot(),
            intex.called_deadline.base_slot(),
            intex.max_call_window_seconds.base_slot(),
            intex.min_call_threshold_seconds.base_slot(),
            intex.forfeit_day_len.base_slot(),
            intex.forfeit_day_live.base_slot(),
            intex.forfeit_day_at.base_slot(),
            intex.called_group_slot.base_slot(),
            intex.forfeit_pass_day.slot(),
            intex.forfeit_cursor.slot(),
            intex.call_latest_day.slot(),
            intex.call_scan_failed_day.base_slot(),
            intex.call_group_currency.base_slot(),
            intex.call_group_called_at.base_slot(),
            intex.cleanup_tree_root.slot(),
            intex.cleanup_tree_mid.base_slot(),
            intex.cleanup_tree_leaf.base_slot(),
            intex.cleanup_day_len.base_slot(),
            intex.cleanup_day_live.base_slot(),
            intex.cleanup_day_at.base_slot(),
            intex.cleanup_group_slot.base_slot(),
            intex.cleanup_deadline.base_slot(),
            intex.cleanup_sweep_day.slot(),
            intex.cleanup_cursor.slot(),
        ];
        for (index, slot) in slots.into_iter().enumerate() {
            assert_eq!(slot, U256::from(11 + index), "field #{index}");
        }
    });
}
