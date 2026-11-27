//! The Rust encoders give the bytes of the golden vectors of `BridgeMsgCodecGolden.t.sol`.

use alloy_primitives::{hex, Address, FixedBytes, U256};
use alloy_sol_types::SolValue;

use crate::errors::IntexFactoryError;
use crate::transport::wire::*;

/// The encoder refused with the typed wire error of `msg_type`.
fn refused<T: std::fmt::Debug>(
    result: Result<T, outbe_primitives::error::PrecompileError>,
    msg_type: u8,
) {
    assert_eq!(
        result.unwrap_err().to_string(),
        outbe_primitives::error::PrecompileError::from(IntexFactoryError::InvalidWireMessage(
            msg_type
        ))
        .to_string()
    );
}

#[test]
fn intex_auction_start_matches_the_solidity_golden() {
    let start = IntexAuctionStart {
        emission_day: 0x11223344,
        commit_end: 0x55667788,
        reveal_end: 0x99AABBCC,
        issuance_end: 0xDDEEFF00,
        promis_load_minor: 0x0102030405060708090A0B0C0D0E0F10,
        min_bid_rate: 0x1A2B3C4D,
        prices: vec![ReferenceCurrencyPrice {
            iso_code: 0xD1D2,
            entry_price_minor: 0x1122334455667788,
            call_price_minor: 0xA1B2C3D4E5F60718,
        }],
        call_notice_period: 0xCAFEBABE,
        call_window: 0x5678,
        call_threshold: 0x9ABC,
        min_bid_quantity: 0xABCD,
        bid_bond_minor: 0xF1F2F3F4F5F6F7F8F9FAFBFCFDFEFF01,
    };
    assert_eq!(
        encode_intex_auction_start(&start).unwrap().to_vec(),
        hex!("0103112233445566778899aabbccddeeff000102030405060708090a0b0c0d0e0f101a2b3c4dcafebabe0000567800009abcabcdf1f2f3f4f5f6f7f8f9fafbfcfdfeff0101d1d21122334455667788a1b2c3d4e5f60718")
    );
}

#[test]
fn intex_auction_start_needs_between_one_and_255_price_rows() {
    let mut start = IntexAuctionStart {
        emission_day: 1,
        commit_end: 0,
        reveal_end: 0,
        issuance_end: 0,
        promis_load_minor: 0,
        min_bid_rate: 0,
        prices: Vec::new(),
        call_notice_period: 0,
        call_window: 0,
        call_threshold: 0,
        min_bid_quantity: 0,
        bid_bond_minor: 0,
    };
    refused(encode_intex_auction_start(&start), MSG_INTEX_AUCTION_START);
    start.prices = vec![
        ReferenceCurrencyPrice {
            iso_code: 840,
            entry_price_minor: 1,
            call_price_minor: 2,
        };
        256
    ];
    refused(encode_intex_auction_start(&start), MSG_INTEX_AUCTION_START);
}

#[test]
fn cancel_clearing_start_and_result_match_the_solidity_goldens() {
    assert_eq!(
        encode_intex_auction_cancel(0x0A0B0C0D, 0x05).to_vec(),
        hex!("010a0a0b0c0d05")
    );
    assert_eq!(
        encode_intex_auction_clearing_start(0x0A0B0C0D).to_vec(),
        hex!("01040a0b0c0d")
    );
    let result = IntexAuctionResult {
        emission_day: 0x11223344,
        won_quantity: 0x55667788,
        won_bids_count: 0x99AABBCC,
        quantity: 0xA1B2C3D4,
        clearing_rate: 0x0102030405060708,
        total_demand: 0xF1F2F3F4F5F6F7F8,
    };
    assert_eq!(
        encode_intex_auction_result(&result).to_vec(),
        hex!("0105112233445566778899aabbcca1b2c3d40102030405060708f1f2f3f4f5f6f7f8")
    );
}

#[test]
fn call_notice_is_the_header_and_the_abi_body() {
    let id = FixedBytes::<14>::from_slice(b"20260212-TRY-U");
    let payload = encode_intex_series_call_notice(20260212, 1_777_000_000, &[id]).unwrap();
    let mut expected = hex!("0108").to_vec();
    expected.extend((20260212u32, 1_777_000_000u64, vec![id]).abi_encode_params());
    assert_eq!(payload.to_vec(), expected);
    refused(
        encode_intex_series_call_notice(1, 1, &[]),
        MSG_INTEX_SERIES_CALL_NOTICE,
    );
    refused(
        encode_intex_series_call_notice(1, 1, &[id; MAX_SERIES_PER_CALL_NOTICE + 1]),
        MSG_INTEX_SERIES_CALL_NOTICE,
    );
}

fn series(day: u32) -> IntexSeriesPayload {
    IntexSeriesPayload {
        seriesId: FixedBytes::from_slice(b"20260212-TRY-U"),
        emissionDay: day,
        issuedAt: 3,
        totalUnits: 4,
        promisLoadMinor: 5,
        entryPriceMinor: 6,
        callNoticePeriod: 7,
        issuanceCurrency: 8,
        referenceCurrency: 9,
        callWindow: 10,
        callThreshold: 11,
        callPriceMinor: 12,
        transitMargin: 13,
    }
}

#[test]
fn series_pages_hold_one_day_and_stay_inside_the_caps() {
    let payload = encode_intex_series(7, 1, 2, &[series(7)]).unwrap();
    assert_eq!(&payload[..2], &[BODY_VERSION_V1, MSG_INTEX_SERIES]);
    let (day, page, total, decoded) =
        <(u32, u16, u16, Vec<IntexSeriesPayload>)>::abi_decode_params(&payload[2..]).unwrap();
    assert_eq!((day, page, total, decoded), (7, 1, 2, vec![series(7)]));
    // After the 4 head words and the length, the 13 fields lie in the order of
    // `BridgeMsgCodec.IntexSeriesPayload`, one word each.
    let words: Vec<U256> = payload[2 + 32 * 5..]
        .chunks(32)
        .map(U256::from_be_slice)
        .collect();
    assert_eq!(words.len(), 13);
    assert_eq!(words[1], U256::from(7u64));
    assert_eq!(
        words[2..13],
        (3u64..=13).map(U256::from).collect::<Vec<_>>()[..]
    );

    refused(encode_intex_series(7, 0, 1, &[series(8)]), MSG_INTEX_SERIES);
    refused(encode_intex_series(7, 1, 1, &[series(7)]), MSG_INTEX_SERIES);
    refused(encode_intex_series(7, 0, 0, &[series(7)]), MSG_INTEX_SERIES);
    refused(encode_intex_series(7, 0, 1, &[]), MSG_INTEX_SERIES);
    refused(
        encode_intex_series(7, 0, 1, &vec![series(7); MAX_SERIES_PER_ISSUANCE + 1]),
        MSG_INTEX_SERIES,
    );
}

#[test]
fn winners_pages_carry_the_partial_fill_inside_the_page() {
    let winners = IntexAuctionWinners {
        emission_day: 7,
        page_index: 0,
        total_pages: 1,
        clearing_rate: 2,
        basis: 3,
        winners: vec![Address::repeat_byte(1), Address::repeat_byte(2)],
        partial_index: 1,
        partial_won: 4,
    };
    let payload = encode_intex_auction_winners(&winners).unwrap();
    let mut expected = vec![BODY_VERSION_V1, MSG_INTEX_AUCTION_WINNERS];
    expected.extend(
        (
            7u32,
            0u16,
            1u16,
            2u64,
            3u128,
            winners.winners.clone(),
            1u16,
            4u16,
        )
            .abi_encode_params(),
    );
    assert_eq!(payload.to_vec(), expected);
    refused(
        encode_intex_auction_winners(&IntexAuctionWinners {
            partial_index: 2,
            ..winners.clone()
        }),
        MSG_INTEX_AUCTION_WINNERS,
    );
    refused(
        encode_intex_auction_winners(&IntexAuctionWinners {
            winners: vec![Address::ZERO; MAX_PAYLOAD_ARRAY_LEN + 1],
            partial_won: 0,
            ..winners
        }),
        MSG_INTEX_AUCTION_WINNERS,
    );
}

#[test]
fn bids_totals_and_pause_decode_the_solidity_goldens() {
    assert_eq!(
        decode_bids_message(&hex!("010211223344000000610003000000c8")),
        Some(BidsMessage::Totals {
            emission_day: 0x11223344,
            src_chain_id: 0x61,
            total_pages: 3,
            total_bids: 0xC8,
        })
    );
    assert_eq!(
        decode_bids_message(&hex!("01090a0b0c0d0102030400110022")),
        Some(BidsMessage::Pause {
            emission_day: 0x0A0B0C0D,
            src_chain_id: 0x01020304,
        })
    );
    assert_eq!(
        decode_bids_message(&hex!("010211223344000000610003000000")),
        None
    );
    assert_eq!(
        decode_bids_message(&hex!("020211223344000000610003000000c8")),
        None
    );
    assert_eq!(decode_bids_message(&hex!("01040a0b0c0d")), None);
}

#[test]
fn a_bids_page_decodes_its_parallel_arrays() {
    let bidders = vec![Address::repeat_byte(7)];
    let packed = vec![U256::from(9u64)];
    let mut payload = vec![BODY_VERSION_V1, MSG_BIDS_PAGE];
    payload.extend((5u32, 97u32, 0u16, 1u16, bidders.clone(), packed.clone()).abi_encode_params());
    assert_eq!(
        decode_bids_message(&payload),
        Some(BidsMessage::Page {
            emission_day: 5,
            src_chain_id: 97,
            page_index: 0,
            total_pages: 1,
            bidders,
            packed_bids: packed,
        })
    );
    let mut uneven = vec![BODY_VERSION_V1, MSG_BIDS_PAGE];
    uneven.extend(
        (
            5u32,
            97u32,
            0u16,
            1u16,
            vec![Address::ZERO; 2],
            vec![U256::ZERO],
        )
            .abi_encode_params(),
    );
    assert_eq!(decode_bids_message(&uneven), None);
}

#[test]
fn the_sender_of_a_hub_message_names_its_chain_only_for_the_expected_account() {
    let router = Address::repeat_byte(0x42);
    // `InteroperableAddress.formatEvmV1(97, router)`, as the testnet registry holds it.
    let mut interop = hex!("000100000161").to_vec();
    interop.push(20);
    interop.extend_from_slice(router.as_slice());
    assert_eq!(evm_chain_of(&interop, router), Some(97));
    assert_eq!(evm_chain_of(&interop, Address::ZERO), None);

    let mut sepolia = hex!("0001000003aa36a7").to_vec();
    sepolia.push(20);
    sepolia.extend_from_slice(router.as_slice());
    assert_eq!(evm_chain_of(&sepolia, router), Some(11_155_111));

    let mut long = hex!("000100000501020304").to_vec();
    long.push(5);
    long.push(20);
    long.extend_from_slice(router.as_slice());
    assert_eq!(evm_chain_of(&long, router), None);
    assert_eq!(evm_chain_of(&interop[..interop.len() - 1], router), None);
}
