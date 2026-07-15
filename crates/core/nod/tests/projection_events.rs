use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_common::WorldwideDay;
use outbe_nod::{precompile::INod, NodBucketState, NodItemState};

fn assert_item_roundtrip(record: NodItemState) {
    let event = INod::NodBodyStored {
        nodId: record.nod_id,
        owner: record.owner,
        gratisLoadMinor: record.gratis_load_minor,
        worldwideDay: record.worldwide_day.into(),
        leagueId: record.league_id,
        floorPriceMinor: record.floor_price_minor,
        bucketKey: record.bucket_key,
        costAmountMinor: record.cost_amount_minor,
        issuanceCurrency: record.issuance_currency,
        referenceCurrency: record.reference_currency,
        issuedAt: record.issued_at,
    };
    let log = event.encode_log_data();
    let decoded = INod::NodBodyStored::decode_log_data(&log).unwrap();
    let reconstructed = NodItemState {
        nod_id: decoded.nodId,
        owner: decoded.owner,
        gratis_load_minor: decoded.gratisLoadMinor,
        worldwide_day: WorldwideDay::new(decoded.worldwideDay),
        league_id: decoded.leagueId,
        floor_price_minor: decoded.floorPriceMinor,
        bucket_key: decoded.bucketKey,
        cost_amount_minor: decoded.costAmountMinor,
        issuance_currency: decoded.issuanceCurrency,
        reference_currency: decoded.referenceCurrency,
        issued_at: decoded.issuedAt,
    };
    assert_eq!(log.topics()[1], B256::from(record.nod_id));
    assert_eq!(reconstructed.nod_id, record.nod_id);
    assert_eq!(reconstructed.owner, record.owner);
    assert_eq!(reconstructed.gratis_load_minor, record.gratis_load_minor);
    assert_eq!(reconstructed.worldwide_day, record.worldwide_day);
    assert_eq!(reconstructed.league_id, record.league_id);
    assert_eq!(reconstructed.floor_price_minor, record.floor_price_minor);
    assert_eq!(reconstructed.bucket_key, record.bucket_key);
    assert_eq!(reconstructed.cost_amount_minor, record.cost_amount_minor);
    assert_eq!(reconstructed.issuance_currency, record.issuance_currency);
    assert_eq!(reconstructed.reference_currency, record.reference_currency);
    assert_eq!(reconstructed.issued_at, record.issued_at);
}

fn assert_bucket_roundtrip(record: NodBucketState) {
    let event = INod::NodBucketBodyStored {
        bucketKey: record.bucket_key,
        worldwideDay: record.worldwide_day.into(),
        floorPriceMinor: record.floor_price_minor,
        isQualified: record.is_qualified,
        totalNods: record.total_nods,
        entryPriceMinor: record.entry_price_minor,
    };
    let log = event.encode_log_data();
    let decoded = INod::NodBucketBodyStored::decode_log_data(&log).unwrap();
    let reconstructed = NodBucketState {
        bucket_key: decoded.bucketKey,
        worldwide_day: WorldwideDay::new(decoded.worldwideDay),
        floor_price_minor: decoded.floorPriceMinor,
        is_qualified: decoded.isQualified,
        total_nods: decoded.totalNods,
        entry_price_minor: decoded.entryPriceMinor,
    };
    assert_eq!(log.topics()[1], record.bucket_key);
    assert_eq!(reconstructed.bucket_key, record.bucket_key);
    assert_eq!(reconstructed.worldwide_day, record.worldwide_day);
    assert_eq!(reconstructed.floor_price_minor, record.floor_price_minor);
    assert_eq!(reconstructed.is_qualified, record.is_qualified);
    assert_eq!(reconstructed.total_nods, record.total_nods);
    assert_eq!(reconstructed.entry_price_minor, record.entry_price_minor);
}

#[test]
fn stored_events_reconstruct_all_nod_boundaries() {
    assert_item_roundtrip(NodItemState {
        nod_id: U256::ZERO,
        owner: Address::ZERO,
        gratis_load_minor: U256::ZERO,
        worldwide_day: WorldwideDay::new(0),
        league_id: 0,
        floor_price_minor: U256::ZERO,
        bucket_key: B256::ZERO,
        cost_amount_minor: U256::ZERO,
        issuance_currency: 0,
        reference_currency: 0,
        issued_at: 0,
    });
    assert_item_roundtrip(NodItemState {
        nod_id: U256::MAX,
        owner: Address::repeat_byte(u8::MAX),
        gratis_load_minor: U256::MAX,
        worldwide_day: WorldwideDay::new(u32::MAX),
        league_id: u16::MAX,
        floor_price_minor: U256::MAX,
        bucket_key: B256::repeat_byte(u8::MAX),
        cost_amount_minor: U256::MAX,
        issuance_currency: u16::MAX,
        reference_currency: u16::MAX,
        issued_at: u64::MAX,
    });
    assert_bucket_roundtrip(NodBucketState {
        bucket_key: B256::ZERO,
        worldwide_day: WorldwideDay::new(0),
        floor_price_minor: U256::ZERO,
        is_qualified: false,
        total_nods: 0,
        entry_price_minor: U256::ZERO,
    });
    assert_bucket_roundtrip(NodBucketState {
        bucket_key: B256::repeat_byte(u8::MAX),
        worldwide_day: WorldwideDay::new(u32::MAX),
        floor_price_minor: U256::MAX,
        is_qualified: true,
        total_nods: u64::MAX,
        entry_price_minor: U256::MAX,
    });
}

#[test]
fn event_signatures_and_deleted_identities_are_pinned() {
    assert_eq!(
        INod::NodBodyStored::SIGNATURE_HASH,
        keccak256(
            "NodBodyStored(uint256,address,uint256,uint32,uint16,uint256,bytes32,uint256,uint16,uint16,uint64)"
        )
    );
    assert_eq!(
        INod::NodBucketBodyStored::SIGNATURE_HASH,
        keccak256("NodBucketBodyStored(bytes32,uint32,uint256,bool,uint64,uint256)")
    );
    assert_eq!(
        INod::NodBodyDeleted::SIGNATURE_HASH,
        keccak256("NodBodyDeleted(uint256)")
    );
    assert_eq!(
        INod::NodBucketBodyDeleted::SIGNATURE_HASH,
        keccak256("NodBucketBodyDeleted(bytes32)")
    );

    let item_log = INod::NodBodyDeleted { nodId: U256::MAX }.encode_log_data();
    assert_eq!(item_log.topics().len(), 2);
    assert!(item_log.data.is_empty());
    assert_eq!(
        INod::NodBodyDeleted::decode_log_data(&item_log)
            .unwrap()
            .nodId,
        U256::MAX
    );
    let bucket_key = B256::repeat_byte(0xab);
    let bucket_log = INod::NodBucketBodyDeleted {
        bucketKey: bucket_key,
    }
    .encode_log_data();
    assert_eq!(bucket_log.topics().len(), 2);
    assert!(bucket_log.data.is_empty());
    assert_eq!(
        INod::NodBucketBodyDeleted::decode_log_data(&bucket_log)
            .unwrap()
            .bucketKey,
        bucket_key
    );
}
