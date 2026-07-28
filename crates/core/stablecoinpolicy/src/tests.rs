use alloy_primitives::{Address, U256};
use outbe_primitives::addresses::STABLECOIN_POLICY_REGISTRY_ADDRESS;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::stablecoin_fork::{
    STABLECOIN_LIST_PAGE_CAP, STABLECOIN_POLICY_MEMBER_BATCH_CAP,
};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::types::StorageKey;
use outbe_primitives::storage::StorageHandle;

use crate::schema::{
    PolicyLane, PolicyType, StablecoinPolicyRegistryContract, ALLOW_ALL_POLICY_ID,
    DENY_ALL_POLICY_ID,
};

fn admin() -> Address {
    Address::repeat_byte(0x11)
}

fn other() -> Address {
    Address::repeat_byte(0x22)
}

fn member(index: u8) -> Address {
    Address::with_last_byte(index)
}

fn revert(error: PrecompileError) -> String {
    match error {
        PrecompileError::Revert(message) => message,
        other => panic!("expected revert, got {other:?}"),
    }
}

fn with_registry(test: impl FnOnce(StorageHandle<'_>, StablecoinPolicyRegistryContract<'_>)) {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        test(
            storage.clone(),
            StablecoinPolicyRegistryContract::new(storage),
        );
    });
}

#[test]
fn v1_layout_and_first_id_are_stable() {
    with_registry(|storage, mut registry| {
        assert_eq!(registry.next_policy_id.slot(), U256::ZERO);
        assert_eq!(registry.policies.base_slot(), U256::from(1u64));
        assert_eq!(registry.member_sets.base_slot(), U256::from(8u64));

        let policy_id = registry
            .create_policy(PolicyType::Whitelist as u8, admin())
            .unwrap();
        assert_eq!(policy_id, U256::from(2u64));
        assert_eq!(
            storage
                .sload(STABLECOIN_POLICY_REGISTRY_ADDRESS, U256::ZERO)
                .unwrap(),
            U256::from(3u64)
        );

        let exists_slot = policy_id.mapping_slot(U256::from(1u64));
        let type_slot = policy_id.mapping_slot(U256::from(2u64));
        let admin_slot = policy_id.mapping_slot(U256::from(3u64));
        assert_eq!(
            storage
                .sload(STABLECOIN_POLICY_REGISTRY_ADDRESS, exists_slot)
                .unwrap(),
            U256::from(1u64)
        );
        assert_eq!(
            storage
                .sload(STABLECOIN_POLICY_REGISTRY_ADDRESS, type_slot)
                .unwrap(),
            U256::from(PolicyType::Whitelist as u8)
        );
        assert_eq!(
            storage
                .sload(STABLECOIN_POLICY_REGISTRY_ADDRESS, admin_slot)
                .unwrap(),
            U256::from_be_slice(admin().as_slice())
        );
    });
}

#[test]
fn builtins_always_exist_and_unknown_views_are_explicit() {
    with_registry(|_, registry| {
        assert!(registry.policy_exists(DENY_ALL_POLICY_ID).unwrap());
        assert!(registry.policy_exists(ALLOW_ALL_POLICY_ID).unwrap());
        assert_eq!(
            registry.policy_type(DENY_ALL_POLICY_ID).unwrap(),
            PolicyType::DenyAll
        );
        assert_eq!(
            registry.policy_type(ALLOW_ALL_POLICY_ID).unwrap(),
            PolicyType::AllowAll
        );
        assert_eq!(
            registry.policy_admin(ALLOW_ALL_POLICY_ID).unwrap(),
            Address::ZERO
        );

        let unknown = U256::from(77u64);
        assert!(!registry.policy_exists(unknown).unwrap());
        assert!(!registry.is_member(unknown, member(1)).unwrap());
        assert!(!registry.can(unknown, member(1), PolicyLane::Send).unwrap());
        assert!(revert(registry.policy_type(unknown).unwrap_err()).contains("unknown policy"));
    });
}

#[test]
fn checked_policy_id_exhaustion_writes_nothing() {
    with_registry(|_, mut registry| {
        registry.next_policy_id.write(U256::MAX).unwrap();
        let error = registry
            .create_policy(PolicyType::Whitelist as u8, admin())
            .unwrap_err();
        assert!(revert(error).contains("exhausted"));
        assert_eq!(registry.next_policy_id.read().unwrap(), U256::MAX);
        assert!(!registry.policies.exists(U256::MAX).unwrap());
    });
}

#[test]
fn simple_policy_truth_tables_are_exact() {
    with_registry(|_, mut registry| {
        let account = member(1);
        let whitelist = registry
            .create_policy(PolicyType::Whitelist as u8, admin())
            .unwrap();
        let blacklist = registry
            .create_policy(PolicyType::Blacklist as u8, admin())
            .unwrap();

        for lane in [PolicyLane::Send, PolicyLane::Receive, PolicyLane::Mint] {
            assert!(!registry.can(DENY_ALL_POLICY_ID, account, lane).unwrap());
            assert!(registry.can(ALLOW_ALL_POLICY_ID, account, lane).unwrap());
            assert!(!registry.can(whitelist, account, lane).unwrap());
            assert!(registry.can(blacklist, account, lane).unwrap());
        }

        registry
            .add_members(whitelist, admin(), &[account])
            .unwrap();
        registry
            .add_members(blacklist, admin(), &[account])
            .unwrap();
        for lane in [PolicyLane::Send, PolicyLane::Receive, PolicyLane::Mint] {
            assert!(registry.can(whitelist, account, lane).unwrap());
            assert!(!registry.can(blacklist, account, lane).unwrap());
        }
    });
}

#[test]
fn directional_policy_selects_one_non_recursive_child_per_lane() {
    with_registry(|_, mut registry| {
        let account = member(1);
        let whitelist = registry
            .create_policy(PolicyType::Whitelist as u8, admin())
            .unwrap();
        registry
            .add_members(whitelist, admin(), &[account])
            .unwrap();

        let directional = registry
            .create_directional_policy(admin(), whitelist, DENY_ALL_POLICY_ID, ALLOW_ALL_POLICY_ID)
            .unwrap();
        assert!(registry
            .can(directional, account, PolicyLane::Send)
            .unwrap());
        assert!(!registry
            .can(directional, account, PolicyLane::Receive)
            .unwrap());
        assert!(registry
            .can(directional, account, PolicyLane::Mint)
            .unwrap());
        assert!(!registry.is_member(directional, account).unwrap());

        let error = registry
            .create_directional_policy(
                admin(),
                directional,
                ALLOW_ALL_POLICY_ID,
                ALLOW_ALL_POLICY_ID,
            )
            .unwrap_err();
        assert!(revert(error).contains("not a valid directional child"));
        let error = registry
            .create_directional_policy(
                admin(),
                U256::from(999u64),
                ALLOW_ALL_POLICY_ID,
                ALLOW_ALL_POLICY_ID,
            )
            .unwrap_err();
        assert!(revert(error).contains("not a valid directional child"));
    });
}

#[test]
fn creation_accepts_only_simple_types_and_nonzero_admin() {
    with_registry(|_, mut registry| {
        for policy_type in [
            PolicyType::DenyAll as u8,
            PolicyType::AllowAll as u8,
            PolicyType::Directional as u8,
            99,
        ] {
            let error = registry.create_policy(policy_type, admin()).unwrap_err();
            assert!(revert(error).contains("invalid policy type"));
        }
        let error = registry
            .create_policy(PolicyType::Whitelist as u8, Address::ZERO)
            .unwrap_err();
        assert!(revert(error).contains("admin must be non-zero"));
        assert_eq!(registry.next_policy_id.read().unwrap(), U256::ZERO);
    });
}

#[test]
fn member_batches_validate_fully_before_writes() {
    with_registry(|_, mut registry| {
        let policy_id = registry
            .create_policy(PolicyType::Whitelist as u8, admin())
            .unwrap();
        let first = member(1);
        registry.add_members(policy_id, admin(), &[first]).unwrap();

        let cases = [
            Vec::new(),
            vec![member(2), Address::ZERO],
            vec![member(2), member(2)],
        ];
        for accounts in cases {
            assert!(registry.add_members(policy_id, admin(), &accounts).is_err());
            assert_eq!(
                registry.policy_member_count(policy_id).unwrap(),
                U256::from(1u64)
            );
            assert!(!registry.is_member(policy_id, member(2)).unwrap());
        }

        let oversized = (0..=STABLECOIN_POLICY_MEMBER_BATCH_CAP)
            .map(|index| Address::with_last_byte((index + 2) as u8))
            .collect::<Vec<_>>();
        assert!(revert(
            registry
                .add_members(policy_id, admin(), &oversized)
                .unwrap_err()
        )
        .contains("exceeds maximum"));
        assert_eq!(
            registry.policy_member_count(policy_id).unwrap(),
            U256::from(1u64)
        );

        let unchanged = registry
            .add_members(policy_id, admin(), &[member(3), first])
            .unwrap_err();
        assert!(revert(unchanged).contains("already true"));
        assert!(!registry.is_member(policy_id, member(3)).unwrap());
    });
}

#[test]
fn member_batch_cap_and_dense_swap_remove_preserve_invariants() {
    with_registry(|_, mut registry| {
        let policy_id = registry
            .create_policy(PolicyType::Whitelist as u8, admin())
            .unwrap();
        let accounts = (1..=STABLECOIN_POLICY_MEMBER_BATCH_CAP)
            .map(|index| Address::with_last_byte(index as u8))
            .collect::<Vec<_>>();
        registry.add_members(policy_id, admin(), &accounts).unwrap();
        assert_eq!(
            registry.policy_member_count(policy_id).unwrap(),
            U256::from(STABLECOIN_POLICY_MEMBER_BATCH_CAP)
        );

        registry
            .remove_members(policy_id, admin(), &[accounts[10], accounts[31]])
            .unwrap();
        assert_eq!(
            registry.policy_member_count(policy_id).unwrap(),
            U256::from(STABLECOIN_POLICY_MEMBER_BATCH_CAP - 2)
        );
        for account in &accounts {
            let expected = *account != accounts[10] && *account != accounts[31];
            assert_eq!(registry.is_member(policy_id, *account).unwrap(), expected);
        }
        let page = registry
            .list_policy_members(policy_id, U256::ZERO, U256::from(STABLECOIN_LIST_PAGE_CAP))
            .unwrap();
        assert_eq!(page.len(), STABLECOIN_POLICY_MEMBER_BATCH_CAP - 2);
        assert!(page
            .iter()
            .all(|account| registry.is_member(policy_id, *account).unwrap()));
    });
}

#[test]
fn membership_requires_current_admin_and_simple_policy() {
    with_registry(|_, mut registry| {
        let simple = registry
            .create_policy(PolicyType::Whitelist as u8, admin())
            .unwrap();
        let directional = registry
            .create_directional_policy(
                admin(),
                ALLOW_ALL_POLICY_ID,
                ALLOW_ALL_POLICY_ID,
                ALLOW_ALL_POLICY_ID,
            )
            .unwrap();
        assert!(revert(
            registry
                .add_members(simple, other(), &[member(1)])
                .unwrap_err()
        )
        .contains("is not admin"));
        assert!(revert(
            registry
                .add_members(directional, admin(), &[member(1)])
                .unwrap_err()
        )
        .contains("no direct membership"));
        assert!(revert(
            registry
                .add_members(ALLOW_ALL_POLICY_ID, admin(), &[member(1)])
                .unwrap_err()
        )
        .contains("immutable"));
    });
}

#[test]
fn membership_unchanged_is_typed_for_add_and_remove() {
    with_registry(|_, mut registry| {
        let policy_id = registry
            .create_policy(PolicyType::Blacklist as u8, admin())
            .unwrap();
        let account = member(1);
        let error = registry
            .remove_members(policy_id, admin(), &[account])
            .unwrap_err();
        assert!(revert(error).contains("already false"));
        registry
            .add_members(policy_id, admin(), &[account])
            .unwrap();
        let error = registry
            .add_members(policy_id, admin(), &[account])
            .unwrap_err();
        assert!(revert(error).contains("already true"));
    });
}

#[test]
fn member_paging_has_only_count_offset_and_bounded_limit() {
    with_registry(|_, mut registry| {
        let policy_id = registry
            .create_policy(PolicyType::Whitelist as u8, admin())
            .unwrap();
        let accounts = (1..=5).map(member).collect::<Vec<_>>();
        registry.add_members(policy_id, admin(), &accounts).unwrap();

        assert_eq!(
            registry
                .list_policy_members(policy_id, U256::from(2u64), U256::from(2u64))
                .unwrap(),
            accounts[2..4]
        );
        assert_eq!(
            registry
                .list_policy_members(policy_id, U256::from(4u64), U256::from(100u64))
                .unwrap(),
            accounts[4..]
        );
        assert!(registry
            .list_policy_members(policy_id, U256::from(5u64), U256::from(1u64))
            .unwrap()
            .is_empty());
        assert!(registry
            .list_policy_members(policy_id, U256::MAX, U256::from(1u64))
            .unwrap()
            .is_empty());

        for invalid in [U256::ZERO, U256::from(101u64)] {
            assert!(revert(
                registry
                    .list_policy_members(policy_id, U256::ZERO, invalid)
                    .unwrap_err()
            )
            .contains("list limit"));
        }
        assert!(revert(
            registry
                .list_policy_members(ALLOW_ALL_POLICY_ID, U256::ZERO, U256::from(1u64))
                .unwrap_err()
        )
        .contains("cannot enumerate"));
    });
}

#[test]
fn admin_transfer_is_two_step_and_stale_nominees_cannot_accept() {
    with_registry(|_, mut registry| {
        let policy_id = registry
            .create_policy(PolicyType::Whitelist as u8, admin())
            .unwrap();
        let first_candidate = other();
        let replacement = Address::repeat_byte(0x33);

        registry
            .begin_policy_admin_transfer(policy_id, admin(), first_candidate)
            .unwrap();
        registry
            .begin_policy_admin_transfer(policy_id, admin(), replacement)
            .unwrap();
        assert_eq!(
            registry.pending_policy_admin(policy_id).unwrap(),
            replacement
        );
        assert!(revert(
            registry
                .accept_policy_admin_transfer(policy_id, first_candidate)
                .unwrap_err()
        )
        .contains("is not pending admin"));

        registry
            .accept_policy_admin_transfer(policy_id, replacement)
            .unwrap();
        assert_eq!(registry.policy_admin(policy_id).unwrap(), replacement);
        assert_eq!(
            registry.pending_policy_admin(policy_id).unwrap(),
            Address::ZERO
        );
        assert!(registry
            .add_members(policy_id, admin(), &[member(1)])
            .is_err());
        registry
            .add_members(policy_id, replacement, &[member(1)])
            .unwrap();
    });
}

#[test]
fn admin_transfer_cancel_clears_candidate() {
    with_registry(|_, mut registry| {
        let policy_id = registry
            .create_policy(PolicyType::Blacklist as u8, admin())
            .unwrap();
        registry
            .begin_policy_admin_transfer(policy_id, admin(), other())
            .unwrap();
        registry
            .cancel_policy_admin_transfer(policy_id, admin())
            .unwrap();
        assert_eq!(
            registry.pending_policy_admin(policy_id).unwrap(),
            Address::ZERO
        );
        assert!(registry
            .accept_policy_admin_transfer(policy_id, other())
            .is_err());
        assert!(registry
            .cancel_policy_admin_transfer(policy_id, admin())
            .is_err());
    });
}

#[test]
fn mixed_member_history_keeps_mapping_count_and_dense_index_in_sync() {
    with_registry(|_, mut registry| {
        let policy_id = registry
            .create_policy(PolicyType::Whitelist as u8, admin())
            .unwrap();
        let universe = (1..=32).map(member).collect::<Vec<_>>();
        let mut expected = std::collections::HashSet::new();
        let mut seed = 0x5eed_u64;

        for _ in 0..256 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let account = universe[(seed as usize) % universe.len()];
            if expected.insert(account) {
                registry
                    .add_members(policy_id, admin(), &[account])
                    .unwrap();
            } else {
                expected.remove(&account);
                registry
                    .remove_members(policy_id, admin(), &[account])
                    .unwrap();
            }

            assert_eq!(
                registry.policy_member_count(policy_id).unwrap(),
                U256::from(expected.len())
            );
            for candidate in &universe {
                assert_eq!(
                    registry.is_member(policy_id, *candidate).unwrap(),
                    expected.contains(candidate)
                );
            }

            let dense = registry
                .list_policy_members(policy_id, U256::ZERO, U256::from(STABLECOIN_LIST_PAGE_CAP))
                .unwrap()
                .into_iter()
                .collect::<std::collections::HashSet<_>>();
            assert_eq!(dense, expected);
        }
    });
}
