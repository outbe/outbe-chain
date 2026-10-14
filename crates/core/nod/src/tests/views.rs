//! Public reads: Nod data, lifecycle state and metadata.

use super::*;

#[test]
fn settled_state_is_exposed_in_nod_data_and_metadata() {
    use crate::precompile::{dispatch, INod};
    use alloy_sol_types::SolCall;
    use base64::Engine;

    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::default();
    let parent = crate::nod_reader(Arc::new(MemoryStorage::new()));
    StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        let item = item(Address::repeat_byte(0x86), U256::from(20), USD);
        api::add_nod(
            &storage,
            outbe_compressed_entities::ExecutionReaders {
                scope: &scope,
                parent: &parent,
            },
            &item,
            U256::from(20),
            crate::test_support::sealed_terms(&storage, &item, U256::from(20)),
        )
        .unwrap();
        qualify(&storage, &item, U256::from(20));
        let bucket_id = WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key);
        api::settle_nod(
            &storage,
            &scope,
            api::load_item(&storage, &scope, &parent, item.nod_id)
                .unwrap()
                .unwrap(),
            api::load_bucket(&storage, &scope, &parent, bucket_id)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let data = dispatch(
            storage.clone(),
            ExecutionReaders {
                scope: &scope,
                parent: &parent,
            },
            &INod::getNodCall {
                nodId: item.nod_id.to_u256(),
            }
            .abi_encode(),
            item.owner,
            U256::ZERO,
        )
        .unwrap();
        assert!(
            INod::getNodCall::abi_decode_returns(&data)
                .unwrap()
                .isSettled
        );
        let data = dispatch(
            storage,
            ExecutionReaders {
                scope: &scope,
                parent: &parent,
            },
            &INod::tokenURICall {
                nodId: item.nod_id.to_u256(),
            }
            .abi_encode(),
            item.owner,
            U256::ZERO,
        )
        .unwrap();
        let uri = INod::tokenURICall::abi_decode_returns(&data).unwrap();
        let json = base64::engine::general_purpose::STANDARD
            .decode(uri.strip_prefix("data:application/json;base64,").unwrap())
            .unwrap();
        assert!(String::from_utf8(json)
            .unwrap()
            .contains("\"trait_type\":\"State\",\"value\":\"Settled\""));
    });
}

#[test]
fn public_lifecycle_reads_use_sealed_terms_and_effective_expiry() {
    use crate::precompile::{dispatch, INod};
    use alloy_sol_types::SolCall;

    // qualified, paid, called_at, notice, now, expected state, expected deadline
    for (qualified, paid, called_at, notice, now, state, deadline) in [
        (false, false, 0, 17, 118, 0, 0),
        (true, false, 0, 17, 118, 1, 0),
        (true, false, 100, 17, 116, 2, 117),
        (true, false, 100, 17, 117, 2, 117),
        (true, false, 100, 17, 118, 4, 117),
        (true, true, 100, 17, 118, 3, 117),
        (true, false, u64::MAX - 1, 17, u64::MAX, 2, u64::MAX),
    ] {
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_timestamp(U256::from(now));
        let scope = ExecutionScope::default();
        let parent = crate::nod_reader(Arc::new(MemoryStorage::new()));
        StorageHandle::enter(&mut provider, |storage| {
            let (item, bucket_id) = seed_public_lifecycle(
                &storage,
                &scope,
                &parent,
                PublicLifecycle {
                    qualified,
                    paid,
                    called_at,
                    notice,
                },
            );
            let call = INod::getNodCall {
                nodId: item.nod_id.to_u256(),
            };
            let bytes = dispatch(
                storage.clone(),
                ExecutionReaders {
                    scope: &scope,
                    parent: &parent,
                },
                &call.abi_encode(),
                item.owner,
                U256::ZERO,
            )
            .unwrap();
            let data = INod::getNodCall::abi_decode_returns(&bytes).unwrap();
            assert_eq!(data.effectiveState, state);
            assert_eq!(data.isQualified, qualified);
            assert_eq!(data.isSettled, paid);
            assert_eq!(data.calledAt, called_at);
            assert_eq!(data.settlementDeadline, deadline);
            assert_sealed_call_terms(&data, notice);
            let bytes = dispatch(
                storage.clone(),
                ExecutionReaders {
                    scope: &scope,
                    parent: &parent,
                },
                &INod::tokenURICall {
                    nodId: item.nod_id.to_u256(),
                }
                .abi_encode(),
                item.owner,
                U256::ZERO,
            )
            .unwrap();
            let uri = INod::tokenURICall::abi_decode_returns(&bytes).unwrap();
            assert_lifecycle_metadata(&uri, state, called_at, paid, deadline);
            // Cleanup removes the public entity instead of retaining a tombstone.
            api::remove_nod(
                &storage,
                &scope,
                api::load_item(&storage, &scope, &parent, item.nod_id)
                    .unwrap()
                    .unwrap(),
                api::load_bucket(&storage, &scope, &parent, bucket_id)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            let error = dispatch(
                storage,
                ExecutionReaders {
                    scope: &scope,
                    parent: &parent,
                },
                &call.abi_encode(),
                item.owner,
                U256::ZERO,
            )
            .unwrap_err();
            assert_removed_nod(error);
        });
    }
}

fn assert_removed_nod(error: outbe_primitives::error::PrecompileError) {
    assert!(
        matches!(error, outbe_primitives::error::PrecompileError::Revert(reason)
                if reason == crate::errors::NodError::NodNotFound.to_string())
    );
}

fn assert_sealed_call_terms(data: &crate::precompile::INod::NodData, notice: u32) {
    assert_eq!(data.callPriceMinor, U256::from(937));
    assert_eq!(
        (
            data.callRate,
            data.callWindow,
            data.callThreshold,
            data.callNoticePeriod
        ),
        (23, 432_000, 172_800, notice)
    );
}

fn assert_lifecycle_metadata(uri: &str, state: u8, called_at: u64, paid: bool, deadline: u64) {
    use base64::Engine;
    let json = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(uri.strip_prefix("data:application/json;base64,").unwrap())
            .unwrap(),
    )
    .unwrap();
    let label = ["Issued", "Qualified", "Called", "Settled", "Forfeited"][usize::from(state)];
    let mut expected = vec![
        format!(r#"{{"trait_type":"State","value":"{label}"}}"#),
        r#"{"trait_type":"Call Price","value":0.000937,"display_type":"number"}"#.to_string(),
    ];
    if called_at != 0 && !paid {
        expected.push(format!(
            r#"{{"trait_type":"Called At","value":{called_at},"display_type":"date"}}"#
        ));
        expected.push(format!(
            r#"{{"trait_type":"Settlement Deadline","value":{deadline},"display_type":"date"}}"#
        ));
    }
    for trait_json in expected {
        assert!(json.contains(&trait_json), "{json}");
    }
}

#[test]
fn qualification_announces_nothing_and_settlement_updates_the_nod() {
    use crate::precompile::INod;
    use alloy_sol_types::SolEvent;

    let parent = crate::nod_reader(Arc::new(MemoryStorage::new()));
    let first = item(Address::repeat_byte(0x51), U256::from(500_000), USD);
    let second = item(Address::repeat_byte(0x52), U256::from(600_000), USD);
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::default();
    StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        for (body, entry) in [(&first, 500_000u64), (&second, 600_000)] {
            api::add_nod(
                &storage,
                outbe_compressed_entities::ExecutionReaders {
                    scope: &scope,
                    parent: &parent,
                },
                body,
                U256::from(entry),
                crate::test_support::sealed_terms(&storage, body, U256::from(entry)),
            )
            .unwrap();
        }
        close_day_above(
            &storage,
            USD,
            crate::pricing::floor_price_minor(U256::from(600_000)).unwrap(),
            first_full_day(first.issued_at),
        );

        let bucket_id = WwdEntityId::from_day_and_digest(first.worldwide_day, first.bucket_key);
        api::settle_nod(
            &storage,
            &scope,
            api::load_item(&storage, &scope, &parent, first.nod_id)
                .unwrap()
                .unwrap(),
            api::load_bucket(&storage, &scope, &parent, bucket_id)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
    });

    let events = provider.get_events(outbe_primitives::addresses::NOD_ADDRESS);
    let batches: Vec<(U256, U256)> = events
        .iter()
        .filter_map(|log| INod::BatchMetadataUpdate::decode_log_data(log).ok())
        .map(|event| (event._fromTokenId, event._toTokenId))
        .collect();
    let updates: Vec<U256> = events
        .iter()
        .filter_map(|log| INod::MetadataUpdate::decode_log_data(log).ok())
        .map(|event| event._tokenId)
        .collect();
    assert!(batches.is_empty(), "a derived qualification writes nothing");
    assert_eq!(updates, vec![first.nod_id.to_u256()]);
}

#[test]
fn token_uri_renders_the_nod_image_and_metadata() {
    use crate::precompile::{dispatch, INod};
    use alloy_sol_types::SolCall;
    use base64::Engine;

    let engine = base64::engine::general_purpose::STANDARD;
    let parent = crate::nod_reader(Arc::new(MemoryStorage::new()));
    let mut body = item(Address::repeat_byte(0x71), U256::from(400_000), USD);
    crate::test_support::set_amount(&mut body, U256::from(1_250_123_456u64));
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::default();
    StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        api::add_nod(
            &storage,
            outbe_compressed_entities::ExecutionReaders {
                scope: &scope,
                parent: &parent,
            },
            &body,
            U256::from(400_000),
            crate::test_support::sealed_terms(&storage, &body, U256::from(400_000)),
        )
        .unwrap();
        let read = || {
            let data = INod::tokenURICall {
                nodId: body.nod_id.to_u256(),
            }
            .abi_encode();
            let out = dispatch(
                storage.clone(),
                ExecutionReaders {
                    scope: &scope,
                    parent: &parent,
                },
                &data,
                Address::ZERO,
                U256::ZERO,
            )
            .unwrap();
            let uri = INod::tokenURICall::abi_decode_returns(&out).unwrap();
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
        };
        let value = |json: &serde_json::Value, name: &str| {
            json["attributes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["trait_type"] == name)
                .map(|entry| entry["value"].clone())
        };

        let (json, svg) = read();
        assert_eq!(value(&json, "State").unwrap(), "Issued");
        assert!(svg.contains(">ISSUED</text>"));
        let row = |label: &str| svg.find(&format!(">{label}</text>")).unwrap();
        assert!(row("Gratis Load") < row("Entry Price"));
        assert!(row("Entry Price") < row("Floor Price"));
        assert!(row("Floor Price") < row("Call Price"));

        qualify(&storage, &body, U256::from(400_000));
        let (json, svg) = read();
        let hex = format!("{:064x}", body.nod_id.to_u256());
        let id = format!("{}-{}", body.worldwide_day, &hex[8..16]);
        assert_eq!(json["name"], format!("Nod {id}"));
        assert_eq!(json["description"], crate::constants::TOKEN_DESCRIPTION);
        assert!(!json.to_string().contains("https://"));
        assert_eq!(value(&json, "State").unwrap(), "Qualified");
        assert_eq!(value(&json, "Worldwide Day").unwrap(), 20_260_715);
        assert_eq!(value(&json, "League").unwrap(), 4);
        assert_eq!(value(&json, "Entry Price").unwrap(), 0.4);
        assert_eq!(value(&json, "Floor Price").unwrap(), 0.432);
        assert_eq!(value(&json, "Call Price").unwrap(), 1.424);
        assert_eq!(
            value(&json, "Encrypted Gratis Load").unwrap(),
            alloy_primitives::hex::encode_prefixed(&body.encrypted.encrypted_gratis_amount)
        );
        assert!(!json.to_string().contains("1250.12"));
        assert!(value(&json, "Settlement Deadline").is_none());

        assert!(svg.contains(">NOD</text>"));
        assert!(svg.contains(&format!(">{id}</text>")));
        assert!(svg.contains(">QUALIFIED</text>"));
        assert!(svg.contains(">1.424</text>"));
        assert!(svg.contains(">Encrypted</text>"));
        assert!(!svg.contains("1,250.12"));
        assert!(!svg.contains("Floor Price"));
    });
}

/// A settled Nod keeps its bucket's call stamp, which must not leak call rows onto the card.
#[test]
fn nod_card_hides_call_rows_it_cannot_honour() {
    use crate::precompile::{dispatch, INod};
    use crate::schema::CallTerms;
    use alloy_sol_types::SolCall;
    use base64::Engine;

    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(118));
    let scope = ExecutionScope::default();
    let parent = crate::nod_reader(Arc::new(MemoryStorage::new()));
    let json = StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        let item = item(Address::repeat_byte(0x88), U256::from(20), USD);
        api::add_nod(
            &storage,
            outbe_compressed_entities::ExecutionReaders {
                scope: &scope,
                parent: &parent,
            },
            &item,
            U256::from(20),
            crate::test_support::sealed_terms(&storage, &item, U256::from(20)),
        )
        .unwrap();
        let mut nod = NodContract::new(storage.clone());
        nod.seal_bucket_call_terms(
            item.bucket_key,
            CallTerms {
                call_price_minor: U256::from(937),
                reference_currency: USD,
                call_rate: 23,
                call_window_seconds: 432_000,
                call_threshold_seconds: 172_800,
                call_notice_period_seconds: 17,
            },
        )
        .unwrap();
        qualify(&storage, &item, U256::from(20));
        let bucket_id = WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key);
        api::settle_nod(
            &storage,
            &scope,
            api::load_item(&storage, &scope, &parent, item.nod_id)
                .unwrap()
                .unwrap(),
            api::load_bucket(&storage, &scope, &parent, bucket_id)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        nod.bucket_called_at.write(&item.bucket_key, 100).unwrap();
        let out = dispatch(
            storage.clone(),
            ExecutionReaders {
                scope: &scope,
                parent: &parent,
            },
            &INod::tokenURICall {
                nodId: item.nod_id.to_u256(),
            }
            .abi_encode(),
            item.owner,
            U256::ZERO,
        )
        .unwrap();
        let uri = INod::tokenURICall::abi_decode_returns(&out).unwrap();
        String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(uri.strip_prefix("data:application/json;base64,").unwrap())
                .unwrap(),
        )
        .unwrap()
    });

    assert!(json.contains(r#"{"trait_type":"State","value":"Settled"}"#));
    assert!(!json.contains("Called At"), "{json}");
    assert!(!json.contains("Settlement Deadline"), "{json}");
}

struct PublicLifecycle {
    qualified: bool,
    paid: bool,
    called_at: u64,
    notice: u32,
}

fn seed_public_lifecycle(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl outbe_compressed_entities::ParentBodySource,
    scenario: PublicLifecycle,
) -> (NodItemState, WwdEntityId) {
    use crate::schema::CallTerms;
    let PublicLifecycle {
        qualified,
        paid,
        called_at,
        notice,
    } = scenario;
    seed_production_nod_genesis(storage);
    begin_block(storage.clone(), scope).unwrap();
    let item = item(Address::repeat_byte(0x87), U256::from(20), USD);
    api::add_nod(
        storage,
        outbe_compressed_entities::ExecutionReaders { scope, parent },
        &item,
        U256::from(20),
        crate::test_support::sealed_terms(storage, &item, U256::from(20)),
    )
    .unwrap();
    let mut nod = NodContract::new(storage.clone());
    nod.seal_bucket_call_terms(
        item.bucket_key,
        CallTerms {
            call_price_minor: U256::from(937),
            reference_currency: USD,
            call_rate: 23,
            call_window_seconds: 432_000,
            call_threshold_seconds: 172_800,
            call_notice_period_seconds: notice,
        },
    )
    .unwrap();
    if qualified {
        qualify(storage, &item, U256::from(20));
    }
    let bucket_id = WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key);
    if paid {
        api::settle_nod(
            storage,
            scope,
            api::load_item(storage, scope, parent, item.nod_id)
                .unwrap()
                .unwrap(),
            api::load_bucket(storage, scope, parent, bucket_id)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
    }
    nod.bucket_called_at
        .write(&item.bucket_key, called_at)
        .unwrap();
    (item, bucket_id)
}
