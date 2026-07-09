//! End-to-end for the Governance precompile driven through its real ABI
//! dispatch (`outbe_governance::precompile::dispatch`) — the same entrypoint the
//! EVM registers at `GOVERNANCE_ADDRESS`. Unlike the crate's unit tests (which
//! call the contract methods directly), this exercises the full path: ABI decode
//! -> dispatch -> caller gating -> ABI encode of the return value.
//!
//! Flow: seed one authority (as genesis does) -> update canon/meta-canon ->
//! submit an OIP -> drive its status Draft -> Approved -> Implemented -> submit a
//! GIP and diff it against the canon. Plus the negative paths: a non-authority
//! cannot write the canon or move a proposal's status.

use alloy_primitives::{address, Address, U256};
use alloy_sol_types::SolCall;
use outbe_governance::precompile::{dispatch as gov_dispatch, IGovernance};
use outbe_governance::GovernanceContract;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

const CHAIN_ID: u64 = 1;
const AUTHORITY: Address = address!("0x1111111111111111111111111111111111111111");
const OUTSIDER: Address = address!("0x2222222222222222222222222222222222222222");
const AUTHOR: Address = address!("0x3333333333333333333333333333333333333333");

// Status codes (mirror `outbe_governance::status`).
const APPROVED: u8 = 1;
const IMPLEMENTED: u8 = 4;

#[test]
fn governance_full_lifecycle_via_dispatch() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        // Seed one authority directly, exactly as the genesis alloc does.
        {
            let gov = GovernanceContract::new(storage.clone());
            gov.authorities.write(&AUTHORITY, true).unwrap();
        }

        // --- canon / meta-canon: authority writes, anyone reads ---
        let call = IGovernance::updateCanonCall {
            text: "line a\nline b\nline c\n".into(),
        };
        let out = gov_dispatch(storage.clone(), &call.abi_encode(), AUTHORITY, U256::ZERO).unwrap();
        let new_version = IGovernance::updateCanonCall::abi_decode_returns(&out).unwrap();
        assert_eq!(new_version, 1);

        let call = IGovernance::updateMetaCanonCall {
            text: "constitution v1".into(),
        };
        gov_dispatch(storage.clone(), &call.abi_encode(), AUTHORITY, U256::ZERO).unwrap();

        let out = gov_dispatch(
            storage.clone(),
            &IGovernance::getCanonCall {}.abi_encode(),
            OUTSIDER,
            U256::ZERO,
        )
        .unwrap();
        let canon = IGovernance::getCanonCall::abi_decode_returns(&out).unwrap();
        assert_eq!(canon.text, "line a\nline b\nline c\n");
        assert_eq!(canon.version, 1);

        // Non-authority cannot overwrite the canon.
        let call = IGovernance::updateCanonCall {
            text: "hostile".into(),
        };
        assert!(gov_dispatch(storage.clone(), &call.abi_encode(), OUTSIDER, U256::ZERO).is_err());

        // --- OIP: anyone submits; authority drives status ---
        let call = IGovernance::submitOipCall {
            text: "proposal body".into(),
        };
        let out = gov_dispatch(storage.clone(), &call.abi_encode(), AUTHOR, U256::ZERO).unwrap();
        let oip_id = IGovernance::submitOipCall::abi_decode_returns(&out).unwrap();
        assert_eq!(oip_id, U256::from(1));

        // Non-authority cannot approve.
        let bad = IGovernance::setOipStatusCall {
            id: oip_id,
            newStatus: APPROVED,
        };
        assert!(gov_dispatch(storage.clone(), &bad.abi_encode(), OUTSIDER, U256::ZERO).is_err());

        // Authority: Draft -> Approved -> Implemented.
        for status in [APPROVED, IMPLEMENTED] {
            let call = IGovernance::setOipStatusCall {
                id: oip_id,
                newStatus: status,
            };
            gov_dispatch(storage.clone(), &call.abi_encode(), AUTHORITY, U256::ZERO).unwrap();
        }

        let out = gov_dispatch(
            storage.clone(),
            &IGovernance::getOipCall { id: oip_id }.abi_encode(),
            OUTSIDER,
            U256::ZERO,
        )
        .unwrap();
        let oip = IGovernance::getOipCall::abi_decode_returns(&out).unwrap();
        assert_eq!(oip.status, IMPLEMENTED);
        assert_eq!(oip.author, AUTHOR);
        assert_eq!(oip.text, "proposal body");

        // --- GIP: independent id sequence + diff against the canon ---
        let call = IGovernance::submitGipCall {
            text: "line a\nline B\nline c\n".into(),
        };
        let out = gov_dispatch(storage.clone(), &call.abi_encode(), AUTHOR, U256::ZERO).unwrap();
        let gip_id = IGovernance::submitGipCall::abi_decode_returns(&out).unwrap();
        assert_eq!(gip_id, U256::from(1)); // GIP ids are separate from OIP ids

        let call = IGovernance::getGipDiffCall {
            id: gip_id,
            base: 0, // 0 = canon
        };
        let out = gov_dispatch(storage.clone(), &call.abi_encode(), OUTSIDER, U256::ZERO).unwrap();
        let diff = IGovernance::getGipDiffCall::abi_decode_returns(&out).unwrap();
        assert!(diff.contains("-line b"), "diff: {diff}");
        assert!(diff.contains("+line B"), "diff: {diff}");

        // counts reflect the separate sequences
        let out = gov_dispatch(
            storage.clone(),
            &IGovernance::oipCountCall {}.abi_encode(),
            OUTSIDER,
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IGovernance::oipCountCall::abi_decode_returns(&out).unwrap(),
            1
        );
    });
}
