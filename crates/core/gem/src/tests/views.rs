//! The precompile surface: ERC-721 views, status, metadata card and the profile.

use alloy_primitives::{Address, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_primitives::addresses::GEM_ADDRESS;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::SECONDS_PER_DAY;
use outbe_primitives::time::SECONDS_PER_DAY_U32;

use super::*;
use crate::config::GemParams;
use crate::precompile::dispatch;
use crate::schema::{GemContract, GemState};

#[test]
fn precompile_transfer_paths_revert() {
    with_storage(|storage| {
        let gem_id = add(storage, sample_params(ALICE));

        let calls: Vec<Vec<u8>> = vec![
            IGem::transferFromCall {
                from: ALICE,
                to: BOB,
                gemId: gem_id,
            }
            .abi_encode(),
            IGem::safeTransferFrom_0Call {
                from: ALICE,
                to: BOB,
                gemId: gem_id,
            }
            .abi_encode(),
            IGem::safeTransferFrom_1Call {
                from: ALICE,
                to: BOB,
                gemId: gem_id,
                data: Default::default(),
            }
            .abi_encode(),
            IGem::approveCall {
                to: BOB,
                gemId: gem_id,
            }
            .abi_encode(),
            IGem::setApprovalForAllCall {
                operator: BOB,
                approved: true,
            }
            .abi_encode(),
        ];

        for data in calls {
            let err = dispatch(storage.clone(), &data, ALICE, U256::ZERO).unwrap_err();
            assert!(
                format!("{err:?}").contains("non-transferable"),
                "expected NonTransferable revert, got {err:?}",
            );
        }
    });
}

#[test]
fn gem_status_reports_call_terms_and_settlement_deadline() {
    with_storage(|storage| {
        let gem_id = add(storage, sample_params(ALICE));
        let terms = view(storage, gem_id).terms;

        let open = status(storage, gem_id);
        assert_eq!(open.callWindow, terms.call_window_seconds);
        assert_eq!(open.callThreshold, terms.call_threshold_seconds);
        assert_eq!(open.callPriceMinor, terms.call_price_minor);
        assert_eq!(open.entryPriceMinor, U256::from(500_000u64));
        assert_eq!(open.settlementDeadline, 0);
        assert_eq!(open.state, GemState::Issued as u8);

        call_gem(storage, gem_id, T_NOW + 10);
        let called = status(storage, gem_id);
        assert_eq!(called.calledAt, T_NOW + 10);
        assert_eq!(called.state, GemState::Called as u8);
        assert_eq!(
            called.settlementDeadline,
            T_NOW + 10 + u64::from(terms.call_notice_period_seconds)
        );
    });
}

#[test]
fn precompile_balance_and_owner_views() {
    with_storage(|storage| {
        let gem_id = add(storage, sample_params(ALICE));

        let out = call(storage, IGem::balanceOfCall { owner: ALICE }.abi_encode()).unwrap();
        assert_eq!(
            IGem::balanceOfCall::abi_decode_returns(&out).unwrap(),
            U256::from(1u64)
        );

        let out = call(storage, IGem::ownerOfCall { gemId: gem_id }.abi_encode()).unwrap();
        assert_eq!(IGem::ownerOfCall::abi_decode_returns(&out).unwrap(), ALICE);

        let out = call(
            storage,
            IGem::tokenOfOwnerByIndexCall {
                owner: ALICE,
                index: U256::ZERO,
            }
            .abi_encode(),
        )
        .unwrap();
        assert_eq!(
            IGem::tokenOfOwnerByIndexCall::abi_decode_returns(&out).unwrap(),
            gem_id
        );
        let err = call(
            storage,
            IGem::tokenOfOwnerByIndexCall {
                owner: ALICE,
                index: U256::from(1u64),
            }
            .abi_encode(),
        )
        .unwrap_err();
        assert!(
            format!("{err:?}").contains("index out of bounds"),
            "{err:?}"
        );

        let out = call(storage, IGem::totalSupplyCall {}.abi_encode()).unwrap();
        assert_eq!(
            IGem::totalSupplyCall::abi_decode_returns(&out).unwrap(),
            U256::from(1u64)
        );
    });
}

/// Global enumeration is gone: the selector is unknown to the ABI.
#[test]
fn token_by_index_is_not_a_selector() {
    with_storage(|storage| {
        add(storage, sample_params(ALICE));
        // keccak256("tokenByIndex(uint256)")[..4]
        let mut data = vec![0x4f, 0x6c, 0xcc, 0xe7];
        data.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());
        assert!(call(storage, data).is_err());
    });
}

#[test]
fn metadata_update_marks_each_lifecycle_transition() {
    let mut provider = provider_at(T_NOW);
    let gem_id = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = add(&storage, sample_params(ALICE));
        call_gem(&storage, gem_id, T_NOW);
        settle(&storage, gem_id);
        burn_settled(&storage, gem_id);
        gem_id
    });

    let updates: Vec<U256> = provider
        .get_events(GEM_ADDRESS)
        .iter()
        .filter_map(|log| IGem::MetadataUpdate::decode_log_data(log).ok())
        .map(|event| event._tokenId)
        .collect();
    assert_eq!(updates, vec![gem_id], "only settlement announces an update");
}

#[test]
fn transfer_logs_announce_mint_and_burn() {
    let mut provider = provider_at(T_NOW);
    let gem_id = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = add(&storage, sample_params(ALICE));
        settle(&storage, gem_id);
        burn_settled(&storage, gem_id);
        gem_id
    });

    let transfers: Vec<(Address, Address, U256)> = provider
        .get_events(GEM_ADDRESS)
        .iter()
        .filter_map(|log| IGem::Transfer::decode_log_data(log).ok())
        .map(|event| (event.from, event.to, event.tokenId))
        .collect();
    assert_eq!(
        transfers,
        vec![
            (Address::ZERO, ALICE, gem_id),
            (ALICE, Address::ZERO, gem_id)
        ]
    );
}

#[test]
fn supported_interfaces_match_the_implemented_selectors() {
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
            IGem::balanceOfCall::SELECTOR,
            IGem::ownerOfCall::SELECTOR,
            IGem::safeTransferFrom_0Call::SELECTOR,
            IGem::safeTransferFrom_1Call::SELECTOR,
            IGem::transferFromCall::SELECTOR,
            IGem::approveCall::SELECTOR,
            IGem::setApprovalForAllCall::SELECTOR,
            IGem::getApprovedCall::SELECTOR,
            IGem::isApprovedForAllCall::SELECTOR,
        ]),
        ERC721_INTERFACE_ID
    );
    assert_eq!(
        interface_id(&[
            IGem::nameCall::SELECTOR,
            IGem::symbolCall::SELECTOR,
            IGem::tokenURICall::SELECTOR,
        ]),
        ERC721_METADATA_INTERFACE_ID
    );
    assert_eq!(IGem::MetadataUpdate::SIGNATURE, "MetadataUpdate(uint256)");
    assert_eq!(
        IGem::BatchMetadataUpdate::SIGNATURE,
        "BatchMetadataUpdate(uint256,uint256)"
    );

    with_storage(|storage| {
        let supports = |id: [u8; 4]| {
            let out = call(
                storage,
                IGem::supportsInterfaceCall {
                    interfaceId: id.into(),
                }
                .abi_encode(),
            )
            .unwrap();
            IGem::supportsInterfaceCall::abi_decode_returns(&out).unwrap()
        };
        for id in [
            ERC165_INTERFACE_ID,
            ERC721_INTERFACE_ID,
            ERC721_METADATA_INTERFACE_ID,
            ERC4906_INTERFACE_ID,
        ] {
            assert!(supports(id), "{id:02x?}");
        }
        assert!(
            !supports(ERC721_ENUMERABLE_INTERFACE_ID),
            "Gem keeps no global id index"
        );
        assert!(!supports(ERC20_INTERFACE_ID));
        assert!(!supports([0xff; 4]));
    });
}

fn token_uri_parts(storage: &StorageHandle, gem_id: U256) -> (serde_json::Value, String) {
    use base64::Engine;

    let engine = base64::engine::general_purpose::STANDARD;
    let out = call(storage, IGem::tokenURICall { gemId: gem_id }.abi_encode()).unwrap();
    let uri = IGem::tokenURICall::abi_decode_returns(&out).unwrap();
    let json = engine
        .decode(uri.strip_prefix("data:application/json;base64,").unwrap())
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&json).unwrap();
    let svg = engine
        .decode(
            json["image"]
                .as_str()
                .unwrap()
                .strip_prefix("data:image/svg+xml;base64,")
                .unwrap(),
        )
        .unwrap();
    (json, String::from_utf8(svg).unwrap())
}

fn trait_value(json: &serde_json::Value, name: &str) -> Option<serde_json::Value> {
    json["attributes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["trait_type"] == name)
        .map(|entry| entry["value"].clone())
}

#[test]
fn token_uri_renders_the_gem_card() {
    with_storage(|storage| {
        set_profile(storage, crate::config::PROFILE_PROD);
        let gem_id = add(storage, sample_params(ALICE));
        let (json, svg) = token_uri_parts(storage, gem_id);

        let id = outbe_common::nft_card::short_id(gem_id);
        assert_eq!(json["name"], format!("Gem {id}"));
        assert_eq!(json["description"], crate::constants::TOKEN_DESCRIPTION);
        assert!(!json.to_string().contains("https://"));
        assert_eq!(trait_value(&json, "State").unwrap(), "Issued");
        assert!(trait_value(&json, "Gem Type").is_none());
        assert_eq!(trait_value(&json, "Entry Price").unwrap(), 0.5);
        assert!(trait_value(&json, "Floor Price").is_none());
        assert_eq!(trait_value(&json, "Call Price").unwrap(), 1.14);
        assert_eq!(trait_value(&json, "Promis Load").unwrap(), 1);
        assert_eq!(trait_value(&json, "Issued At").unwrap(), T_NOW);
        assert!(trait_value(&json, "Call Deadline").is_none());

        assert!(svg.contains(">GEM</text>"));
        assert!(svg.contains(&format!(">{id}</text>")));
        assert!(svg.contains(">ISSUED</text>"));
        assert!(svg.contains(">Call Price</text>"));
        assert!(svg.contains(">1.14</text>"));
        assert!(!svg.contains("Floor Price"));
    });
}

#[test]
fn an_issued_card_leads_with_the_load() {
    with_storage(|storage| {
        let gem_id = add(storage, sample_params(ALICE));
        let (_, svg) = token_uri_parts(storage, gem_id);

        let row = |label: &str| svg.find(&format!(">{label}</text>")).unwrap();
        assert!(row("Promis Load") < row("Entry Price"));
        assert!(row("Entry Price") < row("Call Price"));
    });
}

#[test]
fn a_gem_reads_forfeited_past_the_call_deadline() {
    let mut provider = provider_at(T_NOW);
    let (gem_id, deadline) = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = add(&storage, sample_params(ALICE));
        call_gem(&storage, gem_id, T_NOW);
        let notice = view(&storage, gem_id).terms.call_notice_period_seconds;
        (gem_id, T_NOW + u64::from(notice))
    });

    provider.set_timestamp(U256::from(deadline));
    StorageHandle::enter(&mut provider, |storage| {
        let (json, svg) = token_uri_parts(&storage, gem_id);
        assert_eq!(trait_value(&json, "State").unwrap(), "Called");
        assert_eq!(trait_value(&json, "Called At").unwrap(), T_NOW);
        assert_eq!(trait_value(&json, "Call Deadline").unwrap(), deadline);
        assert!(svg.contains(">CALLED</text>"));
        assert!(svg.contains(&outbe_common::nft_card::timestamp_utc(deadline)));
        assert_eq!(status(&storage, gem_id).state, GemState::Called as u8);
    });

    provider.set_timestamp(U256::from(deadline + 1));
    StorageHandle::enter(&mut provider, |storage| {
        let (json, svg) = token_uri_parts(&storage, gem_id);
        assert_eq!(trait_value(&json, "State").unwrap(), "Forfeited");
        assert_eq!(trait_value(&json, "Call Deadline").unwrap(), deadline);
        assert!(svg.contains(">FORFEITED</text>"));
        assert_eq!(status(&storage, gem_id).state, GemState::Forfeited as u8);
        assert_eq!(
            item(&storage, gem_id).unwrap().state,
            GemState::Issued as u8,
            "the body itself only knows Issued and Settled"
        );
    });
}

/// A settled gem keeps the call stamp it was paid under, even after its deadline.
#[test]
fn a_settled_gem_keeps_its_call_stamp_in_the_view() {
    let mut provider = provider_at(T_NOW);
    let (gem_id, deadline) = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = add(&storage, sample_params(ALICE));
        call_gem(&storage, gem_id, T_NOW);
        settle(&storage, gem_id);
        let notice = view(&storage, gem_id).terms.call_notice_period_seconds;
        (gem_id, T_NOW + u64::from(notice))
    });
    provider.set_timestamp(U256::from(deadline + 1));
    StorageHandle::enter(&mut provider, |storage| {
        let data = status(&storage, gem_id);
        assert_eq!(data.state, GemState::Settled as u8);
        assert_eq!(data.calledAt, T_NOW);
        assert_eq!(data.settlementDeadline, deadline);
    });
}

#[test]
fn config_unset_resolves_by_chain_id() {
    with_storage(|storage| {
        // No genesis profile selected -> resolved by network. The test chain is not mainnet.
        assert_eq!(crate::config::read(storage).unwrap(), GemParams::DEV);
        // An explicit selector still wins over the network default.
        set_profile(storage, crate::config::PROFILE_PROD);
        assert_eq!(crate::config::read(storage).unwrap(), GemParams::PROD);
        assert_eq!(
            GemParams::PROD.call_window_seconds,
            28 * SECONDS_PER_DAY_U32
        );
        assert_eq!(GemParams::PROD.position_validity, 365 * SECONDS_PER_DAY);
    });
}

/// The network default: only mainnet runs the real timings.
#[test]
fn config_auto_profile_follows_the_network() {
    use outbe_primitives::chain::{DEVNET_CHAIN_ID, MAINNET_CHAIN_ID, TESTNET_CHAIN_ID};

    assert_eq!(
        GemParams::from_selector(crate::config::PROFILE_AUTO, MAINNET_CHAIN_ID).unwrap(),
        GemParams::PROD
    );
    for chain_id in [TESTNET_CHAIN_ID, DEVNET_CHAIN_ID, 31_337] {
        assert_eq!(
            GemParams::from_selector(crate::config::PROFILE_AUTO, chain_id).unwrap(),
            GemParams::DEV
        );
    }
}

/// PROD keeps 2.28 times the entry price. DEV uses 1.04 times the entry price.
#[test]
fn config_call_rate_is_pinned_per_profile() {
    assert_eq!(GemParams::PROD.call_rate, crate::constants::CALL_RATE_PCT);
    assert_eq!(GemParams::PROD.call_rate, 128);
    assert_eq!(GemParams::DEV.call_rate, 4);
}

#[test]
fn config_unknown_selector_errors() {
    with_storage(|storage| {
        set_profile(storage, 99u8);
        assert!(crate::config::read(storage).is_err());
    });
}

/// Pins the selector slot the genesis seeder writes (`GEM_PROFILE_SLOT`) and the
/// compact layout around it. Bodies live off-chain, so one field is one slot.
#[test]
fn gem_contract_slot_layout_is_pinned() {
    with_storage(|storage| {
        let gem = GemContract::new(storage.clone());
        assert_eq!(gem.total_supply.slot(), U256::from(0u64));
        assert_eq!(gem.group_gem_count.base_slot(), U256::from(1u64));
        assert_eq!(gem.group_called_at.base_slot(), U256::from(10u64));
        assert_eq!(gem.call_bin_tree_root.base_slot(), U256::from(11u64));
        assert_eq!(gem.expiry_tree_root.slot(), U256::from(19u64));
        assert_eq!(gem.config_profile.slot(), U256::from(32u64));
        assert_eq!(
            gem.min_call_threshold_seconds.base_slot(),
            U256::from(34u64)
        );
        assert_eq!(GemState::Settled as u8, 2);
    });
}
