use alloy_primitives::{address, b256, Address, B256, U256};

use crate::{result::LysisResultV1, ProtocolError, SchemaLimits};

pub const METADOSIS_ADDRESS: Address = address!("000000000000000000000000000000000000100e");

pub const SUBMIT_LYSIS_RESULT_SELECTOR: [u8; 4] = [0x80, 0x58, 0x4e, 0x19];
pub const GET_OFFCHAIN_JOB_SELECTOR: [u8; 4] = [0x4c, 0x13, 0x2d, 0x3d];
pub const GET_OFFCHAIN_VOTE_ACCOUNTABILITY_SELECTOR: [u8; 4] = [0xff, 0x98, 0xdf, 0x7a];
pub const GET_ACTIVE_LYSIS_GENERATION_SELECTOR: [u8; 4] = [0x50, 0xf8, 0xb3, 0xe4];
pub const GET_LYSIS_TERMINAL_RECEIPT_SELECTOR: [u8; 4] = [0x20, 0xf4, 0x6b, 0xe7];
pub const OCOMP_ACTIVATION_REJECTED_SELECTOR: [u8; 4] = [0x8e, 0x34, 0x68, 0x03];
pub const OCOMP_RESULT_VOTE_REJECTED_SELECTOR: [u8; 4] = [0xdd, 0x8b, 0xa0, 0x84];

pub const OCOMP_REQUESTED_TOPIC0: B256 =
    b256!("69a11b11a3b39ad0968d02a67ee3b9e2d790cb9aafbc4de957beed93c39b7dad");
pub const OCOMP_EXPIRED_TOPIC0: B256 =
    b256!("9ca54d8b0ae876fcd3b6e519643b9409e3694c8110d3e17f72f8baa51cea320d");
pub const OCOMP_CONFLICTED_TOPIC0: B256 =
    b256!("4ccae6e58910db3b75ee57048a6f1ef06f25b45ef2564a13ca9ef93aacc371bf");
pub const LYSIS_ACTIVATED_TOPIC0: B256 =
    b256!("f9f846ebfcc5895f442c47e436aeb0a86c170ad48a7863eb9f9dddb0a4492f68");

pub const OCOMP_LIFECYCLE_BEGIN_SELECTOR: [u8; 4] = *b"OSE2";
pub const OCOMP_TERMINAL_REQUEST_SELECTOR: [u8; 4] = *b"OSR2";

pub const OCOMP_ACTIVATION_REJECTION_CODES: core::ops::RangeInclusive<u8> = 1..=17;

/// Encodes the one canonical public ingress for a validator result vote.
pub fn encode_submit_lysis_result_calldata(
    result: &LysisResultV1,
    limits: &SchemaLimits,
) -> Result<Vec<u8>, ProtocolError> {
    let payload = result.encode_canonical(limits)?;
    let vote_cap = usize::try_from(
        crate::generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1.max_result_vote_bytes,
    )
    .map_err(|_| ProtocolError::InvalidInvariant("result vote byte cap"))?;
    if payload.len() > vote_cap {
        return Err(ProtocolError::CapacityExceeded {
            what: "result vote bytes",
            limit: vote_cap,
            actual: payload.len(),
        });
    }
    let padded_len = payload
        .len()
        .checked_add(31)
        .map(|value| value & !31)
        .ok_or(ProtocolError::IntegerOverflow {
            what: "result vote ABI padding",
        })?;
    let total_len = 68_usize
        .checked_add(padded_len)
        .ok_or(ProtocolError::IntegerOverflow {
            what: "result vote ABI calldata",
        })?;
    let mut calldata = vec![0_u8; total_len];
    calldata[..4].copy_from_slice(&SUBMIT_LYSIS_RESULT_SELECTOR);
    calldata[4..36].copy_from_slice(&U256::from(32).to_be_bytes::<32>());
    calldata[36..68].copy_from_slice(&U256::from(payload.len()).to_be_bytes::<32>());
    calldata[68..68 + payload.len()].copy_from_slice(&payload);
    Ok(calldata)
}
