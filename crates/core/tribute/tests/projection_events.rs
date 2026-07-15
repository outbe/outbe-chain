use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_common::WorldwideDay;
use outbe_tribute::{precompile::ITribute, TributeData};

fn assert_roundtrip(record: TributeData) {
    let event = ITribute::TributeBodyStored {
        tokenId: record.token_id,
        owner: record.owner,
        worldwideDay: record.worldwide_day.into(),
        issuanceAmountMinor: record.issuance_amount_minor,
        issuanceCurrency: record.issuance_currency,
        nominalAmountMinor: record.nominal_amount_minor,
        referenceCurrency: record.reference_currency,
        tributePriceMinor: record.tribute_price_minor,
        excludeFromIntexIssuance: record.exclude_from_intex_issuance,
    };
    let log = event.encode_log_data();
    let decoded = ITribute::TributeBodyStored::decode_log_data(&log).unwrap();
    let reconstructed = TributeData {
        token_id: decoded.tokenId,
        owner: decoded.owner,
        worldwide_day: WorldwideDay::new(decoded.worldwideDay),
        issuance_amount_minor: decoded.issuanceAmountMinor,
        issuance_currency: decoded.issuanceCurrency,
        nominal_amount_minor: decoded.nominalAmountMinor,
        reference_currency: decoded.referenceCurrency,
        tribute_price_minor: decoded.tributePriceMinor,
        exclude_from_intex_issuance: decoded.excludeFromIntexIssuance,
    };

    assert_eq!(log.topics()[1], B256::from(record.token_id));
    assert_eq!(reconstructed.token_id, record.token_id);
    assert_eq!(reconstructed.owner, record.owner);
    assert_eq!(reconstructed.worldwide_day, record.worldwide_day);
    assert_eq!(
        reconstructed.issuance_amount_minor,
        record.issuance_amount_minor
    );
    assert_eq!(reconstructed.issuance_currency, record.issuance_currency);
    assert_eq!(
        reconstructed.nominal_amount_minor,
        record.nominal_amount_minor
    );
    assert_eq!(reconstructed.reference_currency, record.reference_currency);
    assert_eq!(
        reconstructed.tribute_price_minor,
        record.tribute_price_minor
    );
    assert_eq!(
        reconstructed.exclude_from_intex_issuance,
        record.exclude_from_intex_issuance
    );
}

#[test]
fn stored_event_reconstructs_all_tribute_boundaries() {
    assert_roundtrip(TributeData {
        token_id: U256::ZERO,
        owner: Address::ZERO,
        worldwide_day: WorldwideDay::new(0),
        issuance_amount_minor: U256::ZERO,
        issuance_currency: 0,
        nominal_amount_minor: U256::ZERO,
        reference_currency: 0,
        tribute_price_minor: U256::ZERO,
        exclude_from_intex_issuance: false,
    });
    assert_roundtrip(TributeData {
        token_id: U256::MAX,
        owner: Address::repeat_byte(u8::MAX),
        worldwide_day: WorldwideDay::new(u32::MAX),
        issuance_amount_minor: U256::MAX,
        issuance_currency: u16::MAX,
        nominal_amount_minor: U256::MAX,
        reference_currency: u16::MAX,
        tribute_price_minor: U256::MAX,
        exclude_from_intex_issuance: true,
    });
}

#[test]
fn deleted_event_signature_and_indexed_identity_are_pinned() {
    assert_eq!(
        ITribute::TributeBodyStored::SIGNATURE_HASH,
        keccak256(
            "TributeBodyStored(uint256,address,uint32,uint256,uint16,uint256,uint16,uint256,bool)"
        )
    );
    assert_eq!(
        ITribute::TributeBodyDeleted::SIGNATURE_HASH,
        keccak256("TributeBodyDeleted(uint256)")
    );
    let event = ITribute::TributeBodyDeleted { tokenId: U256::MAX };
    let log = event.encode_log_data();
    assert_eq!(log.topics().len(), 2);
    assert!(log.data.is_empty());
    assert_eq!(
        ITribute::TributeBodyDeleted::decode_log_data(&log)
            .unwrap()
            .tokenId,
        U256::MAX
    );
}
