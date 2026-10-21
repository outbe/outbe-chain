use super::*;
use crate::VaultRouterContract;
use outbe_primitives::block::BlockContext;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

fn context() -> VoteTargetContext {
    VoteTargetContext {
        proposer: Address::repeat_byte(0xa1),
        attached_value: U256::ZERO,
        block_number: 7,
        chain_id: 1,
    }
}

fn valid_payloads() -> Vec<String> {
    let vault = Address::repeat_byte(0x11);
    let asset = Address::repeat_byte(0x22);
    vec![
        format!(
            r#"{{"operation":"addVault","vault":"{vault:#x}","asset":"{asset:#x}","isoCode":840}}"#
        ),
        format!(r#"{{"operation":"removeVault","vault":"{vault:#x}"}}"#),
        format!(r#"{{"operation":"addLiquiditySource","address":"{vault:#x}","type":4}}"#),
        format!(r#"{{"operation":"removeLiquiditySource","address":"{vault:#x}"}}"#),
        format!(r#"{{"operation":"addLiquidityTarget","address":"{vault:#x}","type":1}}"#),
        format!(r#"{{"operation":"removeLiquidityTarget","address":"{vault:#x}"}}"#),
    ]
}

#[test]
fn all_six_strict_actions_are_valid_without_contract_reads() {
    for payload in valid_payloads() {
        VaultRouterVoteTarget
            .validate(payload.as_bytes(), context())
            .unwrap();
    }
}

#[test]
fn rejects_invalid_shapes_fields_addresses_and_numeric_bounds() {
    let valid = valid_payloads();
    let mut invalid = vec![
        "null".into(),
        "[]".into(),
        "{}".into(),
        "{".into(),
        valid[0].replace("addVault", "unknown"),
        valid[0].replace(",\"isoCode\":840", ""),
        valid[0].replace("840", "65536"),
        valid[0].replace("840", "0"),
        valid[0].replace("840", "-1"),
        valid[0].replace("840", "840.0"),
        valid[0].replace("840", "\"840\""),
        valid[0].replace("840", "null"),
        valid[0].replace("840", "840,\"isoCode\":978"),
        valid[2].replace(":4", ":0"),
        valid[2].replace(":4", ":5"),
        valid[2].replace(":4", ":256"),
        valid[2].replace(":4", ":-1"),
        valid[2].replace(":4", ":1.5"),
        valid[2].replace(":4", ":\"1\""),
        valid[4].replace(":1", ":2"),
    ];
    for payload in &valid {
        invalid.push(payload.replace('}', ",\"extra\":true}"));
        invalid.push(payload.replace(
            "0x1111111111111111111111111111111111111111",
            "0x0000000000000000000000000000000000000000",
        ));
        invalid.push(payload.replace(
            "0x1111111111111111111111111111111111111111",
            "not-an-address",
        ));
        invalid.push(payload.replace("0x1111111111111111111111111111111111111111", "0x11"));
    }
    invalid.push(valid[0].replace(
        "0x2222222222222222222222222222222222222222",
        "0x0000000000000000000000000000000000000000",
    ));
    for payload in &valid[1..] {
        invalid.push(payload.replace(
            '}',
            ",\"asset\":\"0x2222222222222222222222222222222222222222\"}",
        ));
    }
    invalid.push(valid[3].replace('}', ",\"type\":1}"));
    invalid.push(valid[5].replace('}', ",\"type\":1}"));
    for payload in invalid {
        assert!(
            VaultRouterVoteTarget
                .validate(payload.as_bytes(), context())
                .is_err(),
            "accepted {payload}"
        );
    }
}

#[test]
fn corrupt_stored_payloads_propagate_instead_of_becoming_proposal_error() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(30, 1_700_000_000, 1), storage);
        for payload in [
            "{",
            "{}",
            r#"{"operation":"removeVault","vault":"0x0000000000000000000000000000000000000000"}"#,
        ] {
            assert!(VaultRouterVoteTarget
                .handle_approved(&ctx, U256::ONE, payload.as_bytes(), context())
                .is_err());
        }
    });
    assert!(provider.storage.is_empty());
    assert!(provider.sub_call_inputs().is_empty());
}

#[test]
fn provider_failure_propagates_and_rolls_back_partial_route_writes() {
    let payload = &valid_payloads()[2];
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(30, 1_700_000_000, 1), storage);
        assert_eq!(
            VaultRouterVoteTarget
                .handle_approved(&ctx, U256::ONE, payload.as_bytes(), context())
                .unwrap(),
            TargetExecutionOutcome::Applied
        );
    });
    let mutations = provider.clear_mutation_failure();
    assert!(mutations > 0);
    // Include failure after the event has been appended, not just early writes.
    for operation in 0..mutations {
        let mut provider = HashMapStorageProvider::new(1);
        provider.fail_after_mutation_at(operation);
        StorageHandle::enter(&mut provider, |storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(30, 1_700_000_000, 1),
                storage.clone(),
            );
            assert!(
                matches!(
                    VaultRouterVoteTarget.handle_approved(
                        &ctx,
                        U256::ONE,
                        payload.as_bytes(),
                        context()
                    ),
                    Err(PrecompileError::Storage(_))
                ),
                "operation {operation}"
            );
            let router = VaultRouterContract::new(storage);
            assert_eq!(router.liquidity_sources.len().unwrap(), 0);
            assert_eq!(
                router
                    .liquidity_source_types
                    .read(&Address::repeat_byte(0x11))
                    .unwrap(),
                0
            );
        });
        assert!(provider.get_events(VAULT_ROUTER_ADDRESS).is_empty());
    }
}

#[test]
fn failed_supersession_restores_previous_approval_and_index() {
    let vault = Address::repeat_byte(0x11);
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        state::record_approved_vault_action(&storage, U256::ONE, vault, state::ADD_VAULT, 7)
            .unwrap();
    });
    provider.fail_after_mutation_at(1);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(30, 1_700_000_000, 1),
            storage.clone(),
        );
        assert!(matches!(
            VaultRouterVoteTarget.handle_approved(
                &ctx,
                U256::from(2),
                valid_payloads()[1].as_bytes(),
                context()
            ),
            Err(PrecompileError::Storage(_))
        ));
        let router = VaultRouterContract::new(storage);
        assert_eq!(
            router.pending_vault_proposals.read(&vault).unwrap(),
            U256::ONE
        );
        assert_eq!(
            router
                .approved_vault_actions
                .get(U256::ONE)
                .unwrap()
                .unwrap()
                .kind,
            state::ADD_VAULT
        );
        assert!(router
            .approved_vault_actions
            .get(U256::from(2))
            .unwrap()
            .is_none());
    });
    assert!(provider.sub_call_inputs().is_empty());
}
