//! The Settled Intex ERC-1155 collection on the precompile.

use super::*;
use alloy_sol_types::SolEvent;
use base64::{engine::general_purpose::STANDARD, Engine};
use outbe_primitives::addresses::{INTEX_ADDRESS, INTEX_FACTORY_ADDRESS};

fn other() -> Address {
    Address::repeat_byte(0xB2)
}

fn call<C: SolCall>(s: &StorageHandle, call: C) -> C::Return {
    let out = dispatch(s.clone(), &call.abi_encode(), Address::ZERO, U256::ZERO).unwrap();
    C::abi_decode_returns(&out).unwrap()
}

fn balance(s: &StorageHandle, account: Address, id: U256) -> U256 {
    call(s, IIntex::balanceOfCall { account, id })
}

#[test]
fn balances_follow_each_owners_settlements_and_minings() {
    with_registry(|s| {
        api::create_series(&s, sample_params(7)).unwrap();
        let id = crate::schema::intex_id(sid(7));
        api::record_settled_units(&s, sid(7), owner(), 30).unwrap();
        api::record_settled_units(&s, sid(7), other(), 10).unwrap();
        api::record_mined_units(&s, sid(7), owner(), 12).unwrap();

        assert_eq!(balance(&s, owner(), id), U256::from(18));
        assert_eq!(balance(&s, other(), id), U256::from(10));
        assert_eq!(balance(&s, owner(), U256::MAX), U256::ZERO);
        assert_eq!(
            call(
                &s,
                IIntex::balanceOfBatchCall {
                    accounts: vec![owner(), other()],
                    ids: vec![id, id],
                }
            ),
            vec![U256::from(18), U256::from(10)]
        );
        let mismatched = IIntex::balanceOfBatchCall {
            accounts: vec![owner()],
            ids: vec![id, id],
        };
        assert!(dispatch(
            s.clone(),
            &mismatched.abi_encode(),
            Address::ZERO,
            U256::ZERO
        )
        .is_err());
    });
}

#[test]
fn settled_units_never_move() {
    with_registry(|s| {
        api::create_series(&s, sample_params(7)).unwrap();
        let id = crate::schema::intex_id(sid(7));
        api::record_settled_units(&s, sid(7), owner(), 5).unwrap();

        let calls = [
            IIntex::setApprovalForAllCall {
                operator: other(),
                approved: true,
            }
            .abi_encode(),
            IIntex::safeTransferFromCall {
                from: owner(),
                to: other(),
                id,
                value: U256::ONE,
                data: Default::default(),
            }
            .abi_encode(),
            IIntex::safeBatchTransferFromCall {
                from: owner(),
                to: other(),
                ids: vec![id],
                values: vec![U256::ONE],
                data: Default::default(),
            }
            .abi_encode(),
        ];
        for data in calls {
            assert!(dispatch(s.clone(), &data, owner(), U256::ZERO).is_err());
        }
        assert!(!call(
            &s,
            IIntex::isApprovedForAllCall {
                account: owner(),
                operator: other(),
            }
        ));
        assert_eq!(balance(&s, owner(), id), U256::from(5));
    });
}

#[test]
fn the_collection_reports_erc1155_with_metadata_uri() {
    with_registry(|s| {
        let supports = |interface: [u8; 4]| {
            call(
                &s,
                IIntex::supportsInterfaceCall {
                    interfaceId: interface.into(),
                },
            )
        };
        assert!(supports([0x01, 0xff, 0xc9, 0xa7]));
        assert!(supports([0xd9, 0xb6, 0x7a, 0x26]));
        assert!(supports([0x0e, 0x89, 0x34, 0x1c]));
        assert!(!supports([0x80, 0xac, 0x58, 0xcd]));
        assert_eq!(call(&s, IIntex::nameCall {}), "Settled Intex");
        assert_eq!(call(&s, IIntex::symbolCall {}), "SINTEX");
    });
}

#[test]
fn the_uri_shows_the_series_settled_card() {
    with_registry(|s| {
        api::create_series(&s, sample_params(7)).unwrap();
        let uri = call(
            &s,
            IIntex::uriCall {
                id: crate::schema::intex_id(sid(7)),
            },
        );
        let json = STANDARD
            .decode(uri.strip_prefix("data:application/json;base64,").unwrap())
            .unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(doc["name"], format!("Settled Intex {}", sid(7)));
        assert!(doc["image"]
            .as_str()
            .unwrap()
            .starts_with("data:image/svg+xml;base64,"));
        assert!(doc["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["trait_type"] == "State" && t["value"] == "Settled"));

        let missing = IIntex::uriCall {
            id: crate::schema::intex_id(sid(8)),
        };
        assert!(dispatch(s.clone(), &missing.abi_encode(), Address::ZERO, U256::ZERO).is_err());
    });
}

#[test]
fn settlement_mints_and_mining_burns_the_settled_units() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT));
    StorageHandle::enter(&mut storage, |s| {
        api::create_series(&s, sample_params(7)).unwrap();
        api::record_settled_units(&s, sid(7), owner(), 30).unwrap();
        api::record_mined_units(&s, sid(7), owner(), 12).unwrap();
    });

    let transfers: Vec<_> = storage
        .get_events(INTEX_ADDRESS)
        .iter()
        .filter_map(|log| IIntex::TransferSingle::decode_log_data(log).ok())
        .map(|event| (event.operator, event.from, event.to, event.id, event.value))
        .collect();
    let id = crate::schema::intex_id(sid(7));
    assert_eq!(
        transfers,
        vec![
            (
                INTEX_FACTORY_ADDRESS,
                Address::ZERO,
                owner(),
                id,
                U256::from(30)
            ),
            (
                INTEX_FACTORY_ADDRESS,
                owner(),
                Address::ZERO,
                id,
                U256::from(12)
            ),
        ]
    );
}
