use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{SolCall, SolError};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::precompile::{dispatch, IAttestersRegistry};
use crate::schema::AttestersRegistryContract;

const ADMIN: Address = Address::repeat_byte(0xAA);
const ATT1: Address = Address::repeat_byte(0x11);
const ATT2: Address = Address::repeat_byte(0x22);
const ATT3: Address = Address::repeat_byte(0x33);
const ATT4: Address = Address::repeat_byte(0x44);

fn revert_selector(err: PrecompileError) -> [u8; 4] {
    match err {
        PrecompileError::RevertBytes(bytes) if bytes.len() >= 4 => {
            bytes[..4].try_into().expect("selector")
        }
        other => panic!("expected typed revert, got {other:?}"),
    }
}

fn provider_at_block(block: u64) -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_block_number(block);
    provider
}

#[test]
fn register_permissions_revoke_and_reregister() {
    let mut provider = provider_at_block(10);
    StorageHandle::enter(&mut provider, |storage| {
        let mut reg = AttestersRegistryContract::new(storage.clone());
        reg.bootstrap_admin(ADMIN).unwrap();

        // Non-admin rejected.
        let err = dispatch(
            storage.clone(),
            &IAttestersRegistry::registerCall {
                attester: ATT1,
                permissionMask: 0b0001,
                isRotationCandidate: false,
                consensusKey: B256::ZERO,
                p2pAddr: U256::ZERO,
            }
            .abi_encode(),
            ATT1,
            U256::ZERO,
        )
        .unwrap_err();
        assert_eq!(revert_selector(err), IAttestersRegistry::NotAdmin::SELECTOR);

        // Register ATT1 with SubmitSpendingUnit bit (bit 0).
        dispatch(
            storage.clone(),
            &IAttestersRegistry::registerCall {
                attester: ATT1,
                permissionMask: 1 << 0,
                isRotationCandidate: false,
                consensusKey: B256::ZERO,
                p2pAddr: U256::ZERO,
            }
            .abi_encode(),
            ADMIN,
            U256::ZERO,
        )
        .unwrap();

        assert!(reg.is_active(ATT1).unwrap());
        assert!(reg
            .has_permission(ATT1, IAttestersRegistry::PermissionBit::SubmitSpendingUnit)
            .unwrap());
        assert!(!reg
            .has_permission(ATT1, IAttestersRegistry::PermissionBit::Heartbeat)
            .unwrap());
        assert_eq!(reg.all_actors().unwrap(), vec![ATT1]);

        // Update mask to Heartbeat (bit 3).
        reg.update_mask(ADMIN, ATT1, 1 << 3).unwrap();
        assert!(!reg
            .has_permission(ATT1, IAttestersRegistry::PermissionBit::SubmitSpendingUnit)
            .unwrap());
        assert!(reg
            .has_permission(ATT1, IAttestersRegistry::PermissionBit::Heartbeat)
            .unwrap());

        // Revoke disables active and permissions, keeps actor entry.
        reg.revoke(ADMIN, ATT1).unwrap();
        assert!(!reg.is_active(ATT1).unwrap());
        assert!(!reg
            .has_permission(ATT1, IAttestersRegistry::PermissionBit::Heartbeat)
            .unwrap());
        assert_eq!(reg.actor_count().unwrap(), U256::from(1u64));

        // Re-register does not duplicate actor in all_actors.
        reg.register(ADMIN, ATT1, 1 << 0, false, B256::ZERO, U256::ZERO)
            .unwrap();
        assert!(reg.is_active(ATT1).unwrap());
        assert_eq!(reg.all_actors().unwrap(), vec![ATT1]);
    });
}

#[test]
fn force_reshare_and_quorum_activation() {
    let mut provider = provider_at_block(5);
    StorageHandle::enter(&mut provider, |storage| {
        let mut reg = AttestersRegistryContract::new(storage.clone());
        reg.bootstrap_admin(ADMIN).unwrap();

        for (addr, key_byte) in [(ATT3, 3u8), (ATT1, 1), (ATT4, 4), (ATT2, 2)] {
            reg.register(
                ADMIN,
                addr,
                0xF,
                true,
                B256::repeat_byte(key_byte),
                U256::from(key_byte),
            )
            .unwrap();
        }
        assert_eq!(
            reg.rotation_candidates_sorted().unwrap(),
            vec![ATT1, ATT2, ATT3, ATT4]
        );
        // 4 - (4-1)/3 = 3
        assert_eq!(reg.attestation_quorum().unwrap(), 3);

        let gen = reg.force_reshare(ADMIN, vec![ATT4]).unwrap();
        assert_eq!(gen, 1);
        assert_eq!(reg.reshare_skip_dealers.read_all().unwrap(), vec![ATT4]);
        assert_eq!(reg.activated_generation.read().unwrap(), 0);

        let output = B256::repeat_byte(0xEE);
        reg.attest_generation(ATT1, 1, output).unwrap();
        reg.attest_generation(ATT2, 1, output).unwrap();
        assert_eq!(reg.activated_generation.read().unwrap(), 0);
        assert_eq!(reg.effective_attest_count(1, output).unwrap(), 2);

        // Duplicate attestation reverts AlreadyAttested.
        let err = reg.attest_generation(ATT2, 1, output).unwrap_err();
        assert_eq!(
            revert_selector(err),
            IAttestersRegistry::AlreadyAttested::SELECTOR
        );

        // Revoking ATT4 shrinks quorum from 3 -> 3 - 2/3 = 3 (still 3), wait: for n=3, 3 - 2/3 = 3;
        // revoking ATT3 as well shrinks n=2 -> quorum = 2 - 1/3 = 2, triggering activation!
        reg.revoke(ADMIN, ATT4).unwrap();
        assert_eq!(reg.attestation_quorum().unwrap(), 3);
        assert_eq!(reg.activated_generation.read().unwrap(), 0);

        reg.revoke(ADMIN, ATT3).unwrap();
        assert_eq!(reg.attestation_quorum().unwrap(), 2);
        assert_eq!(reg.activated_generation.read().unwrap(), 1);
        assert_eq!(reg.activated_output_hash.read().unwrap(), output);
    });
}

#[test]
fn admin_two_step_transfer() {
    let mut provider = provider_at_block(1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut reg = AttestersRegistryContract::new(storage.clone());
        reg.bootstrap_admin(ADMIN).unwrap();

        let new_admin = Address::repeat_byte(0xBB);
        reg.initiate_admin_transfer(ADMIN, new_admin).unwrap();
        assert_eq!(reg.pending_admin.read().unwrap(), new_admin);

        let err = reg.accept_admin_transfer(ATT1).unwrap_err();
        assert_eq!(revert_selector(err), IAttestersRegistry::NotAdmin::SELECTOR);

        reg.accept_admin_transfer(new_admin).unwrap();
        assert_eq!(reg.admin.read().unwrap(), new_admin);
        assert_eq!(reg.pending_admin.read().unwrap(), Address::ZERO);
    });
}
