//! Issue an Intex series straight into the engine, without running an auction.
//!
//! The node exposes `issueForTest` only under its `e2e-test` feature; the entry
//! makes the two calls the clearing engine makes, so the series is indexed for
//! the qualify sweep and its mints travel as real issuance instructions.

use alloy_primitives::{Address, FixedBytes, U256};
use alloy_sol_types::sol;
use eyre::{eyre, Result};

use crate::internal::eth;

/// `IntexFactory`, the engine precompile.
pub const INTEX_FACTORY: Address =
    alloy_primitives::address!("0x0000000000000000000000000000000000001015");

sol! {
    interface IIntexFactoryTestArming {
        function issueForTest(
            bytes14 seriesId,
            uint32 worldwideDay,
            uint32 issuedIntexCount,
            uint128 promisLoadMinor,
            uint256 entryPriceMinor,
            uint16 issuanceCurrency,
            uint16 referenceCurrency,
            address[] recipients,
            uint256[] quantities,
            uint32[] recipientChains,
            uint32[] snapshotChains
        ) external;
    }

    interface IIntexSettlement {
        function settle(bytes14 seriesId, address intexHolder, uint256 amount, address paymentToken) external;
        function quoteCostAmount(bytes14 seriesId, address paymentToken) external view returns (uint256 costAmountMinor);
    }

    interface IPromisMining {
        function minePromis(bytes14 seriesId, uint256 amount, uint256 nonce, bytes32 mac, uint64 opNonce)
            external
            returns (uint256 promisAmount);
    }

    interface ITestToken {
        function mint(address to, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
    }
}

/// One series to issue. `issuance` is its three-byte currency code, which with the
/// reference byte spells the id — `20260824-USD-U`.
#[derive(Clone, Copy, Debug)]
pub struct SeriesSpec {
    pub issuance: [u8; 3],
    pub issuance_currency: u16,
}

/// The 14 ASCII bytes of `20260824-USD-U`.
fn series_id(worldwide_day: u32, issuance: [u8; 3], reference: u8) -> FixedBytes<14> {
    let mut bytes = [b'-'; 14];
    let mut day = worldwide_day;
    for slot in bytes[..8].iter_mut().rev() {
        *slot = b'0' + (day % 10) as u8;
        day /= 10;
    }
    bytes[9..12].copy_from_slice(&issuance);
    bytes[13] = reference;
    FixedBytes::from(bytes)
}

/// Issue every spec into the same worldwide day and reference currency, so the
/// sweeps see one group rather than a group per series.
#[allow(clippy::too_many_arguments)]
pub fn issue_series(
    url: &str,
    sender_key: &str,
    worldwide_day: u32,
    reference_currency: u16,
    reference_byte: u8,
    entry_price_minor: U256,
    promis_load_minor: u128,
    holder: Address,
    units: u32,
    chain_id: u32,
    specs: &[SeriesSpec],
) -> Result<Vec<FixedBytes<14>>> {
    let mut issued = Vec::with_capacity(specs.len());
    for spec in specs {
        let id = series_id(worldwide_day, spec.issuance, reference_byte);
        eth::send_call(
            url,
            INTEX_FACTORY,
            sender_key,
            &IIntexFactoryTestArming::issueForTestCall {
                seriesId: id,
                worldwideDay: worldwide_day,
                issuedIntexCount: units,
                promisLoadMinor: promis_load_minor,
                entryPriceMinor: entry_price_minor,
                issuanceCurrency: spec.issuance_currency,
                referenceCurrency: reference_currency,
                recipients: vec![holder],
                quantities: vec![U256::from(units)],
                recipientChains: vec![chain_id],
                snapshotChains: vec![chain_id],
            },
            None,
        )
        .map_err(|error| eyre!("issueForTest was refused: {error}"))?;
        issued.push(id);
    }
    Ok(issued)
}

/// Give `holder` enough of `asset` to settle with, and let the engine pull it.
pub fn fund_settler(url: &str, asset: Address, holder_key: &str, amount: U256) -> Result<()> {
    let signer: alloy_signer_local::PrivateKeySigner = holder_key
        .parse()
        .map_err(|error| eyre!("invalid holder key: {error}"))?;
    let holder = alloy_signer::Signer::address(&signer);
    eth::send_call(
        url,
        asset,
        holder_key,
        &ITestToken::mintCall { to: holder, amount },
        None,
    )?;
    eth::send_call(
        url,
        asset,
        holder_key,
        &ITestToken::approveCall {
            spender: INTEX_FACTORY,
            amount,
        },
        None,
    )?;
    Ok(())
}

/// What one unit of `series` costs in `payment_token`'s minor units. Reverts on a
/// token the series does not accept, which is the check worth failing loudly.
pub fn quote_cost(url: &str, series: FixedBytes<14>, payment_token: Address) -> Option<U256> {
    eth::read_call(
        url,
        INTEX_FACTORY,
        &IIntexSettlement::quoteCostAmountCall {
            seriesId: series,
            paymentToken: payment_token,
        },
    )
}

/// Settle `amount` units of `series` held by the caller, paying in `payment_token`.
pub fn settle(
    url: &str,
    holder_key: &str,
    series: FixedBytes<14>,
    holder: Address,
    amount: u32,
    payment_token: Address,
) -> Result<()> {
    eth::send_call(
        url,
        INTEX_FACTORY,
        holder_key,
        &IIntexSettlement::settleCall {
            seriesId: series,
            intexHolder: holder,
            amount: U256::from(amount),
            paymentToken: payment_token,
        },
        None,
    )
    .map_err(|error| eyre!("settle was refused: {error}"))?;
    Ok(())
}

/// The SHA-256 preimage `validate_pow` rebuilds: the hex spelling of holder, amount,
/// series and sequence, then the nonce's own eight bytes.
fn pow_hash(
    holder: Address,
    promis_amount: U256,
    series: FixedBytes<14>,
    seq: u32,
    nonce: u64,
) -> [u8; 32] {
    use sha2::{Digest as _, Sha256};
    let mut preimage = String::new();
    preimage.push_str(&alloy_primitives::hex::encode(holder.as_slice()));
    preimage.push_str(&alloy_primitives::hex::encode(
        promis_amount.to_be_bytes::<32>(),
    ));
    preimage.push_str(&alloy_primitives::hex::encode(series.as_slice()));
    preimage.push_str(&alloy_primitives::hex::encode(seq.to_be_bytes()));
    let mut data = preimage.into_bytes();
    data.extend_from_slice(&nonce.to_be_bytes());
    Sha256::digest(&data).into()
}

/// A nonce whose hash carries the one leading zero byte the engine demands.
pub fn mine_nonce(
    holder: Address,
    promis_amount: U256,
    series: FixedBytes<14>,
    seq: u32,
) -> Option<u64> {
    (0_u64..1_000_000).find(|nonce| pow_hash(holder, promis_amount, series, seq, *nonce)[0] == 0)
}

/// Burn settled units into Promis. `mac` authorizes the confidential mint, and the
/// proof of work is what the engine checks before it will call it at all.
#[allow(clippy::too_many_arguments)]
pub fn mine_promis(
    url: &str,
    holder_key: &str,
    series: FixedBytes<14>,
    amount: u32,
    nonce: u64,
    mac: [u8; 32],
    op_nonce: u64,
) -> Result<()> {
    eth::send_call(
        url,
        INTEX_FACTORY,
        holder_key,
        &IPromisMining::minePromisCall {
            seriesId: series,
            amount: U256::from(amount),
            nonce: U256::from(nonce),
            mac: mac.into(),
            opNonce: op_nonce,
        },
        None,
    )
    .map_err(|error| eyre!("minePromis was refused: {error}"))?;
    Ok(())
}
