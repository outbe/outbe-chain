//! The ERC-721 surface: soulbound transfers, transfer logs and interfaces.

use super::*;

#[test]
fn transfer_surface_is_soulbound() {
    use crate::precompile::{dispatch, INod};
    use alloy_sol_types::SolCall;

    let owner = Address::repeat_byte(0x41);
    let other = Address::repeat_byte(0x42);
    let token = U256::from(7);
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let call = |data: Vec<u8>| dispatch(storage.clone(), &data, owner, U256::ZERO);
        for data in [
            INod::transferFromCall {
                from: owner,
                to: other,
                nodId: token,
            }
            .abi_encode(),
            INod::safeTransferFrom_0Call {
                from: owner,
                to: other,
                nodId: token,
            }
            .abi_encode(),
            INod::safeTransferFrom_1Call {
                from: owner,
                to: other,
                nodId: token,
                data: Default::default(),
            }
            .abi_encode(),
            INod::approveCall {
                to: other,
                nodId: token,
            }
            .abi_encode(),
            INod::setApprovalForAllCall {
                operator: other,
                approved: true,
            }
            .abi_encode(),
        ] {
            let err = call(data).unwrap_err();
            assert!(format!("{err:?}").contains("non-transferable"), "{err:?}");
        }
        let out = call(INod::getApprovedCall { nodId: token }.abi_encode()).unwrap();
        assert_eq!(
            INod::getApprovedCall::abi_decode_returns(&out).unwrap(),
            Address::ZERO
        );
        let out = call(
            INod::isApprovedForAllCall {
                owner,
                operator: other,
            }
            .abi_encode(),
        )
        .unwrap();
        assert!(!INod::isApprovedForAllCall::abi_decode_returns(&out).unwrap());
    });
}

#[test]
fn transfer_logs_announce_issuance_and_removal() {
    use crate::precompile::INod;
    use alloy_sol_types::SolEvent;

    let owner = Address::repeat_byte(0x61);
    let body = item(owner, U256::from(5), USD);
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);
        crate::test_support::add(&storage, &body).unwrap();
        let group_id = body.group_key;
        api::remove_nod(
            &storage,
            api::load_item(&storage, body.nod_id).unwrap().unwrap(),
            api::load_group(&storage, group_id).unwrap().unwrap(),
        )
        .unwrap();
    });

    let nod_id = body.nod_id.to_u256();
    let transfers: Vec<(Address, Address, U256)> = provider
        .get_events(outbe_primitives::addresses::NOD_ADDRESS)
        .iter()
        .filter_map(|log| INod::Transfer::decode_log_data(log).ok())
        .map(|event| (event.from, event.to, event.tokenId))
        .collect();
    assert_eq!(
        transfers,
        vec![
            (Address::ZERO, owner, nod_id),
            (owner, Address::ZERO, nod_id)
        ]
    );
}

#[test]
fn supported_interfaces_match_the_implemented_selectors() {
    use crate::precompile::{dispatch, INod};
    use alloy_sol_types::SolCall;
    use outbe_primitives::erc::{
        ERC165_INTERFACE_ID, ERC20_INTERFACE_ID, ERC4906_INTERFACE_ID,
        ERC721_ENUMERABLE_INTERFACE_ID, ERC721_INTERFACE_ID, ERC721_METADATA_INTERFACE_ID,
    };

    let interface_id = |selectors: &[[u8; 4]]| {
        selectors.iter().fold([0u8; 4], |acc, selector| {
            std::array::from_fn(|i| acc[i] ^ selector[i])
        })
    };
    assert_eq!(
        interface_id(&[
            INod::balanceOfCall::SELECTOR,
            INod::ownerOfCall::SELECTOR,
            INod::safeTransferFrom_0Call::SELECTOR,
            INod::safeTransferFrom_1Call::SELECTOR,
            INod::transferFromCall::SELECTOR,
            INod::approveCall::SELECTOR,
            INod::setApprovalForAllCall::SELECTOR,
            INod::getApprovedCall::SELECTOR,
            INod::isApprovedForAllCall::SELECTOR,
        ]),
        ERC721_INTERFACE_ID
    );
    assert_eq!(
        interface_id(&[
            INod::nameCall::SELECTOR,
            INod::symbolCall::SELECTOR,
            INod::tokenURICall::SELECTOR,
        ]),
        ERC721_METADATA_INTERFACE_ID
    );
    assert_eq!(
        interface_id(&[
            INod::totalSupplyCall::SELECTOR,
            INod::tokenByIndexCall::SELECTOR,
            INod::tokenOfOwnerByIndexCall::SELECTOR,
        ]),
        ERC721_ENUMERABLE_INTERFACE_ID
    );

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let supports = |id: [u8; 4]| {
            let data = INod::supportsInterfaceCall {
                interfaceId: id.into(),
            }
            .abi_encode();
            let out = dispatch(storage.clone(), &data, Address::ZERO, U256::ZERO).unwrap();
            INod::supportsInterfaceCall::abi_decode_returns(&out).unwrap()
        };
        for id in [
            ERC165_INTERFACE_ID,
            ERC721_INTERFACE_ID,
            ERC721_METADATA_INTERFACE_ID,
            ERC721_ENUMERABLE_INTERFACE_ID,
            ERC4906_INTERFACE_ID,
        ] {
            assert!(supports(id), "{id:02x?}");
        }
        assert!(!supports(ERC20_INTERFACE_ID));
        assert!(!supports([0xff; 4]));
    });
}
