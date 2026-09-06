//! Issue an Intex series straight into the engine, without running an auction.
//!
//! The node exposes `issueForTest` only under its `e2e-test` feature; the entry
//! makes the two calls the clearing engine makes, so the series is indexed for
//! the qualify sweep and its mints travel as real issuance instructions.

use alloy_primitives::{Address, FixedBytes, U256};
use alloy_sol_types::sol;
use eyre::{eyre, Result};

use crate::internal::eth;

/// Send `call` and prove the receipt says success: a reverted transaction still
/// returns a hash, so an unchecked send hides the failure it was meant to catch.
fn send_checked<C: alloy_sol_types::SolCall>(
    url: &str,
    to: Address,
    key: &str,
    call: &C,
    label: &str,
) -> Result<()> {
    let tx = eth::send_call(url, to, key, call, None)
        .map_err(|error| eyre!("{label} was not sent: {error}"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        if let Some(receipt) = eth::receipt_json(url, &tx) {
            let status = receipt
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if status == "0x1" {
                return Ok(());
            }
            return Err(eyre!("{label} reverted: {receipt}"));
        }
        if std::time::Instant::now() >= deadline {
            return Err(eyre!("{label} never produced a receipt"));
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// `IntexFactory`, the engine precompile.
pub const INTEX_FACTORY: Address =
    alloy_primitives::address!("0x0000000000000000000000000000000000001015");

sol! {
    interface IIntexFactoryTestArming {
        function seedDayVwapsForTest(uint16 isoCode, uint32 days, uint256 value) external;
        function issueForTest(
            bytes14[] seriesIds,
            uint16[] issuanceCurrencies,
            uint32 worldwideDay,
            uint32 issuedAt,
            uint32 issuedIntexCount,
            uint128 promisLoadMinor,
            uint256 entryPriceMinor,
            uint16 referenceCurrency,
            address[] recipients,
            uint256[] quantities,
            uint32[] recipientChains,
            uint32[] snapshotChains
        ) external;
    }

    interface IIntexSettlement {
        function settle(bytes14 seriesId, address intexHolder, uint256 amount, bytes payNoteProof) external;
        function quoteSettlement(bytes14 seriesId, address paymentToken) external view returns (uint16 settlementCurrency, uint256 payableUnits);
    }

    struct ReferenceCurrencyPrice {
        uint16 isoCode;
        uint64 entryPriceMinor;
        uint64 floorPriceMinor;
        uint64 callPriceMinor;
    }

    struct AuctionStageStartParams {
        uint32 worldwideDay;
        uint32 commitEnd;
        uint32 revealEnd;
        uint32 issuanceEnd;
        uint128 promisLoadMinor;
        uint32 minIntexBidRate;
        ReferenceCurrencyPrice[] prices;
        uint32 callNoticePeriod;
        uint32 callWindow;
        uint32 callThreshold;
        uint16 minIntexBidQuantity;
        uint128 commitBondMinor;
        uint8 dayState;
    }

    interface IOriginRouterStart {
        function sendAuctionStageStart(AuctionStageStartParams params) external payable;
    }

    interface IPromisMining {
        function minePromis(bytes14 seriesId, uint256 amount, uint64 nonce, bytes32 mac, uint64 opNonce)
            external
            returns (uint256 promisAmount);
    }

    struct SendParam {
        uint32 dstChainId;
        bytes32 to;
        uint256 tokenId;
        uint256 amount;
    }

    struct BatchSendParam {
        uint32 dstChainId;
        bytes32 to;
        uint256[] tokenIds;
        uint256[] amounts;
    }

    interface IIntexNFT1155Bridge {
        function setRemoteMessenger(uint32 chainId, bytes interop) external;
        function quoteBatchSend(BatchSendParam sendParam) external view returns (uint256 fee);
        function batchSend(BatchSendParam sendParam) external payable returns (bytes32 sendId);
        function quoteSend(SendParam sendParam) external view returns (uint256 fee);
        function send(SendParam sendParam) external payable returns (bytes32 sendId);
    }

    interface ITestToken {
        function mint(address to, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
    }
}

/// One series to issue. `issuance` is its three-byte currency code, which with the
/// reference byte spells the id - `20260824-USD-U`.
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
///
/// `units_per_chain` is parallel to `chains`: the holder ends up with that many
/// units of every series on each chain named there.
#[allow(clippy::too_many_arguments)]
pub fn issue_series(
    url: &str,
    sender_key: &str,
    worldwide_day: u32,
    issued_at: u32,
    reference_currency: u16,
    reference_byte: u8,
    entry_price_minor: U256,
    promis_load_minor: u128,
    holder: Address,
    units_per_chain: &[u32],
    chains: &[u32],
    specs: &[SeriesSpec],
) -> Result<Vec<FixedBytes<14>>> {
    let units: u32 = units_per_chain.iter().sum();
    let ids: Vec<FixedBytes<14>> = specs
        .iter()
        .map(|spec| series_id(worldwide_day, spec.issuance, reference_byte))
        .collect();

    // One call for the whole day: the engine counts issuance chunks over the legs it
    // is handed, so a second send would announce a one-chunk day twice.
    send_checked(
        url,
        INTEX_FACTORY,
        sender_key,
        &IIntexFactoryTestArming::issueForTestCall {
            seriesIds: ids.clone(),
            issuanceCurrencies: specs.iter().map(|spec| spec.issuance_currency).collect(),
            worldwideDay: worldwide_day,
            issuedAt: issued_at,
            issuedIntexCount: units,
            promisLoadMinor: promis_load_minor,
            entryPriceMinor: entry_price_minor,
            referenceCurrency: reference_currency,
            // One recipient leg per chain: the holder ends up with units on each,
            // which is what makes bringing them home a real step later.
            recipients: vec![holder; chains.len()],
            quantities: units_per_chain.iter().copied().map(U256::from).collect(),
            recipientChains: chains.to_vec(),
            snapshotChains: chains.to_vec(),
        },
        "issueForTest",
    )?;
    Ok(ids)
}

/// Give `holder` enough of `asset` to settle with, and let the engine pull it.
pub fn fund_settler(url: &str, asset: Address, holder_key: &str, amount: U256) -> Result<()> {
    let signer: alloy_signer_local::PrivateKeySigner = holder_key
        .parse()
        .map_err(|error| eyre!("invalid holder key: {error}"))?;
    let holder = alloy_signer::Signer::address(&signer);
    send_checked(
        url,
        asset,
        holder_key,
        &ITestToken::mintCall { to: holder, amount },
        "mint the settlement asset",
    )?;
    send_checked(
        url,
        asset,
        holder_key,
        &ITestToken::approveCall {
            spender: crate::internal::addresses::PAYNOTE_ADDR,
            amount,
        },
        "approve the note pool",
    )?;
    Ok(())
}

/// What one unit of `series` costs in `payment_token`'s minor units. Reverts on a
/// token the series does not accept, which is the check worth failing loudly.
pub fn quote_cost(url: &str, series: FixedBytes<14>, payment_token: Address) -> Option<U256> {
    eth::read_call(
        url,
        INTEX_FACTORY,
        &IIntexSettlement::quoteSettlementCall {
            seriesId: series,
            paymentToken: payment_token,
        },
    )
    .map(|quote| quote.payableUnits)
}

/// Settle `amount` units of `series` held by the caller. The proof carries the
/// asset, so this takes no payment token.
pub fn settle(
    url: &str,
    holder_key: &str,
    series: FixedBytes<14>,
    holder: Address,
    amount: u32,
    paynote_proof: &[u8],
) -> Result<()> {
    send_checked(
        url,
        INTEX_FACTORY,
        holder_key,
        &IIntexSettlement::settleCall {
            seriesId: series,
            intexHolder: holder,
            amount: U256::from(amount),
            payNoteProof: paynote_proof.to_vec().into(),
        },
        "settle",
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
    // The engine hashes the raw bytes, so anything else mines a nonce it will
    // reject: SHA256(holder ++ amount_be32 ++ seriesId ++ seq_be4 ++ nonce_be8).
    let mut data = Vec::with_capacity(20 + 32 + 14 + 4 + 8);
    data.extend_from_slice(holder.as_slice());
    data.extend_from_slice(&promis_amount.to_be_bytes::<32>());
    data.extend_from_slice(series.as_slice());
    data.extend_from_slice(&seq.to_be_bytes());
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
    send_checked(
        url,
        INTEX_FACTORY,
        holder_key,
        &IPromisMining::minePromisCall {
            seriesId: series,
            amount: U256::from(amount),
            nonce,
            mac: mac.into(),
            opNonce: op_nonce,
        },
        "minePromis",
    )
    .map_err(|error| eyre!("minePromis was refused: {error}"))?;
    Ok(())
}

/// Freeze the day's target set, which the router does at STAGE_START and without
/// which it refuses to address an issuance leg to any chain.
#[allow(clippy::too_many_arguments)]
pub fn open_day(
    url: &str,
    sender_key: &str,
    origin_router: Address,
    worldwide_day: u32,
    now: u32,
    reference_currency: u16,
    entry_price_minor: u64,
    promis_load_minor: u128,
) -> Result<()> {
    // Stage ends sit in the past: the scenario issues directly, so no bid ever races
    // them, and a day whose windows are open would only invite one.
    let params = AuctionStageStartParams {
        worldwideDay: worldwide_day,
        commitEnd: now,
        revealEnd: now,
        issuanceEnd: now,
        promisLoadMinor: promis_load_minor,
        minIntexBidRate: 0,
        prices: vec![ReferenceCurrencyPrice {
            isoCode: reference_currency,
            entryPriceMinor: entry_price_minor,
            floorPriceMinor: entry_price_minor,
            callPriceMinor: entry_price_minor,
        }],
        callNoticePeriod: 0,
        callWindow: 0,
        callThreshold: 0,
        minIntexBidQuantity: 1,
        commitBondMinor: 0,
        dayState: 1,
    };
    send_checked(
        url,
        origin_router,
        sender_key,
        &IOriginRouterStart::sendAuctionStageStartCall { params },
        "sendAuctionStageStart",
    )
    .map_err(|error| eyre!("sendAuctionStageStart was refused: {error}"))?;
    Ok(())
}

/// Fill the last `days` closed UTC days with `value`, exactly as this module's own
/// tests do: the per-day value plus the finalized watermark. The call sweep reads
/// those days and counts how many cleared its trigger.
pub fn seed_day_vwaps(
    url: &str,
    sender_key: &str,
    iso_code: u16,
    days: u32,
    value: U256,
) -> Result<()> {
    send_checked(
        url,
        INTEX_FACTORY,
        sender_key,
        &IIntexFactoryTestArming::seedDayVwapsForTestCall {
            isoCode: iso_code,
            days,
            value,
        },
        "seedDayVwapsForTest",
    )
}

/// Bring `amount` units of `series` home from the chain `bridge` lives on.
///
/// While a series is tradable the hop may change hands; once it is Called only a
/// move to the holder's own address is allowed, so `to` is always the holder here.
pub fn bridge_home(
    url: &str,
    holder_key: &str,
    bridge: Address,
    home_chain_id: u32,
    token_id: U256,
    holder: Address,
    amount: u32,
) -> Result<()> {
    let params = SendParam {
        dstChainId: home_chain_id,
        to: FixedBytes::<32>::left_padding_from(holder.as_slice()),
        tokenId: token_id,
        amount: U256::from(amount),
    };
    let fee = eth::read_call(
        url,
        bridge,
        &IIntexNFT1155Bridge::quoteSendCall {
            sendParam: params.clone(),
        },
    )
    .ok_or_else(|| eyre!("the bridge would not quote the hop"))?;

    let tx = eth::send_call(
        url,
        bridge,
        holder_key,
        &IIntexNFT1155Bridge::sendCall { sendParam: params },
        Some(fee),
    )
    .map_err(|error| eyre!("bridge send was not accepted: {error}"))?;
    let _ = tx;
    Ok(())
}

/// ERC-7930 interoperable address: a version tag, the chain reference at its own
/// minimal width, then the address. Mirrors `InteroperableAddress.formatEvmV1`.
fn interop_address(chain_id: u64, address: Address) -> Vec<u8> {
    let reference: Vec<u8> = chain_id
        .to_be_bytes()
        .into_iter()
        .skip_while(|byte| *byte == 0)
        .collect();
    let mut out = vec![0x00, 0x01, 0x00, 0x00];
    out.push(u8::try_from(reference.len()).expect("a chain reference is at most 8 bytes"));
    out.extend_from_slice(&reference);
    out.push(20);
    out.extend_from_slice(address.as_slice());
    out
}

/// Tell `bridge` where its peer lives on `peer_chain_id`. Without it the bridge
/// cannot even quote a hop, let alone send one.
pub fn set_remote_messenger(
    url: &str,
    sender_key: &str,
    bridge: Address,
    peer_chain_id: u64,
    peer: Address,
) -> Result<()> {
    send_checked(
        url,
        bridge,
        sender_key,
        &IIntexNFT1155Bridge::setRemoteMessengerCall {
            chainId: u32::try_from(peer_chain_id).map_err(|_| eyre!("chain id exceeds uint32"))?,
            interop: interop_address(peer_chain_id, peer).into(),
        },
        "setRemoteMessenger",
    )
}

/// Bring several series home in one message, the way a holder with more than one
/// would: a single burn set on this side and a single mint set at home.
pub fn batch_bridge_home(
    url: &str,
    holder_key: &str,
    bridge: Address,
    home_chain_id: u32,
    holder: Address,
    tokens: &[(U256, u32)],
) -> Result<()> {
    let params = BatchSendParam {
        dstChainId: home_chain_id,
        to: FixedBytes::<32>::left_padding_from(holder.as_slice()),
        tokenIds: tokens.iter().map(|(id, _)| *id).collect(),
        amounts: tokens
            .iter()
            .map(|(_, amount)| U256::from(*amount))
            .collect(),
    };
    let fee = eth::read_call(
        url,
        bridge,
        &IIntexNFT1155Bridge::quoteBatchSendCall {
            sendParam: params.clone(),
        },
    )
    .ok_or_else(|| eyre!("the bridge would not quote the batch hop"))?;

    eth::send_call(
        url,
        bridge,
        holder_key,
        &IIntexNFT1155Bridge::batchSendCall { sendParam: params },
        Some(fee),
    )
    .map_err(|error| eyre!("batch bridge send was not accepted: {error}"))?;
    Ok(())
}
