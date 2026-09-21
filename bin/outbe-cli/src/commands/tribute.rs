//! Tribute commands.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use clap::Subcommand;
use eyre::Result;
use outbe_l2_zk_canonical::claims::tribute::PUBLIC_INPUT_COUNT;
use outbe_l2_zk_canonical::{combined_len, Claim};
use outbe_primitives::time::WorldwideDay;
use serde_json::Value;

use crate::abi::{
    IL2Registry, ITeeRegistry, ITribute, ITributeFactory, L2_REGISTRY_ADDRESS, TEE_REGISTRY_ADDR,
    TRIBUTE_ADDR, TRIBUTE_FACTORY_ADDR,
};
use crate::rpc::Rpc;

const TOKEN_URI_JSON_PREFIX: &str = "data:application/json;utf8,";

type DayTotalsReturn = <ITribute::getDayTotalsCall as SolCall>::Return;
type TokenIdsReturn = <ITribute::getTributesByOwnerCall as SolCall>::Return;

fn canonical_amount_base(value: &str) -> std::result::Result<String, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| "amount must be a canonical unsigned u64".to_owned())?;
    if parsed.to_string() != value {
        return Err("amount must be a canonical unsigned u64".to_owned());
    }
    Ok(value.to_owned())
}

fn canonical_amount_micro(value: &str) -> std::result::Result<String, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| "amount_micro must be a canonical unsigned u64 below 1000000".to_owned())?;
    if parsed.to_string() != value || parsed >= 1_000_000 {
        return Err("amount_micro must be a canonical unsigned u64 below 1000000".to_owned());
    }
    Ok(value.to_owned())
}

#[derive(Subcommand)]
pub enum TributeCmd {
    /// Show tribute metadata via tokenURI JSON
    Show {
        /// Tribute token ID (`0x`-hex)
        token_id: U256,
    },
    /// Show aggregate totals for a WorldwideDay
    DayTotals {
        /// WorldwideDay value
        worldwide_day: WorldwideDay,
    },
    /// Show tribute token IDs owned by an address
    ByOwner {
        /// Owner address
        owner: Address,
    },
    /// Show tribute token IDs recorded for a WorldwideDay
    ByDay {
        /// WorldwideDay value
        worldwide_day: WorldwideDay,
    },
    /// Show total Tribute supply
    Supply,
    /// Show current owner for a Tribute token ID
    Owner {
        /// Tribute token ID (`0x`-hex)
        token_id: U256,
    },
    /// Submit an encrypted tribute offer (decrypted inside the SGX enclave).
    /// Encrypts to the DKG-derived offer key registered in the TeeRegistry and
    /// sends `offerTribute`; requires `--private-key` and the ZK offer inputs
    /// (`--zk-proof`, `--zk-merkle-root`, `--signature`).
    Offer {
        /// WorldwideDay (must be in OFFERING status), e.g. 20241220
        worldwide_day: WorldwideDay,
        /// Issuance amount in whole units (`amount_base`)
        #[arg(long, default_value = "100", value_parser = canonical_amount_base)]
        amount: String,
        /// Six-decimal raw remainder (`amount_micro`, 0..999999)
        #[arg(long, default_value = "0", value_parser = canonical_amount_micro)]
        amount_micro: String,
        /// ISO 4217 currency code (840 = USD)
        #[arg(long, default_value_t = 840)]
        currency: u16,
        /// Exclude the resulting Tribute from Intex issuance
        #[arg(long, default_value_t = false)]
        exclude_from_intex_issuance: bool,
        /// L2 zkMerkleRoot bytes (`0x`-hex). Required; `0x` is accepted only for
        /// a deliberate negative transaction, which the node then rejects.
        #[arg(long)]
        zk_merkle_root: String,
        /// Combined Tribute proof bytes (`0x`-hex), including its four public
        /// inputs. Verifies under the circuit version registered for `--l2-chain-id`.
        /// Required; `0x` is accepted only for a deliberate negative
        /// transaction, which the node then rejects.
        #[arg(long)]
        zk_proof: String,
        /// L2 chain id selecting the circuit that verifies `--zk-proof`. Must
        /// match the caller's registered L2. Defaults to the caller's
        /// registered chain id when `--zk-proof` is non-empty.
        #[arg(long)]
        l2_chain_id: Option<u32>,
        /// Exact circuit version registered for `--l2-chain-id`. Defaults, when
        /// `--zk-proof` is non-empty, to the one registered tribute circuit
        /// whose proof length matches the supplied proof.
        #[arg(long)]
        circuit_version: Option<String>,
        /// Exact 32-byte TributeDraft id (`0x`-hex) used to construct
        /// `nft_hash` and `binding_hash`. Required with `--zk-proof`; otherwise
        /// generated randomly.
        #[arg(long)]
        tribute_draft_id: Option<String>,
        /// Exact 32-byte SpendingUnit hash (`0x`-hex) included in the
        /// TributeDraft. Required with `--zk-proof`; otherwise generated
        /// randomly.
        #[arg(long)]
        su_hash: Option<String>,
        /// BLS MinSig signature (compressed G1, 48 bytes, `0x`-hex) over `--zk-merkle-root`
        /// produced with the network key registered in the L2Registry. Required;
        /// `0x` is accepted only for a deliberate negative transaction, which the
        /// node then rejects.
        #[arg(long)]
        signature: String,
    },
}

impl TributeCmd {
    pub async fn run(self, client: &(impl Rpc + Sync), private_key: Option<&str>) -> Result<()> {
        match self {
            Self::Show { token_id } => show(client, token_id).await,
            Self::DayTotals { worldwide_day } => day_totals(client, worldwide_day).await,
            Self::ByOwner { owner } => by_owner(client, owner).await,
            Self::ByDay { worldwide_day } => by_day(client, worldwide_day).await,
            Self::Supply => supply(client).await,
            Self::Owner { token_id } => owner(client, token_id).await,
            Self::Offer {
                worldwide_day,
                amount,
                amount_micro,
                currency,
                exclude_from_intex_issuance,
                zk_merkle_root,
                zk_proof,
                l2_chain_id,
                circuit_version,
                tribute_draft_id,
                su_hash,
                signature,
            } => {
                offer(
                    client,
                    private_key,
                    worldwide_day,
                    amount,
                    amount_micro,
                    currency,
                    exclude_from_intex_issuance,
                    &zk_merkle_root,
                    &zk_proof,
                    l2_chain_id,
                    circuit_version.as_deref(),
                    tribute_draft_id.as_deref(),
                    su_hash.as_deref(),
                    &signature,
                )
                .await
            }
        }
    }
}

async fn fetch_total_supply(client: &(impl Rpc + Sync)) -> Result<U256> {
    let result = client
        .eth_call(TRIBUTE_ADDR, &ITribute::totalSupplyCall {}.abi_encode())
        .await?;
    Ok(ITribute::totalSupplyCall::abi_decode_returns(&result)?)
}

async fn fetch_owner_of(client: &(impl Rpc + Sync), token_id: U256) -> Result<Address> {
    let call = ITribute::ownerOfCall {
        tributeId: token_id,
    };
    let result = client.eth_call(TRIBUTE_ADDR, &call.abi_encode()).await?;
    Ok(ITribute::ownerOfCall::abi_decode_returns(&result)?)
}

async fn fetch_token_uri(client: &(impl Rpc + Sync), token_id: U256) -> Result<String> {
    let call = ITribute::tokenURICall {
        tributeId: token_id,
    };
    let result = client.eth_call(TRIBUTE_ADDR, &call.abi_encode()).await?;
    Ok(ITribute::tokenURICall::abi_decode_returns(&result)?)
}

async fn fetch_day_totals(
    client: &(impl Rpc + Sync),
    worldwide_day: WorldwideDay,
) -> Result<DayTotalsReturn> {
    let call = ITribute::getDayTotalsCall {
        worldwideDay: worldwide_day.into(),
    };
    let result = client.eth_call(TRIBUTE_ADDR, &call.abi_encode()).await?;
    Ok(ITribute::getDayTotalsCall::abi_decode_returns(&result)?)
}

async fn fetch_tributes_by_owner(
    client: &(impl Rpc + Sync),
    owner: Address,
) -> Result<TokenIdsReturn> {
    let call = ITribute::getTributesByOwnerCall { owner };
    let result = client.eth_call(TRIBUTE_ADDR, &call.abi_encode()).await?;
    Ok(ITribute::getTributesByOwnerCall::abi_decode_returns(
        &result,
    )?)
}

async fn fetch_tributes_by_day(
    client: &(impl Rpc + Sync),
    worldwide_day: WorldwideDay,
) -> Result<TokenIdsReturn> {
    let call = ITribute::getTributesByDayCall {
        worldwideDay: worldwide_day.into(),
    };
    let result = client.eth_call(TRIBUTE_ADDR, &call.abi_encode()).await?;
    Ok(ITribute::getTributesByDayCall::abi_decode_returns(&result)?)
}

async fn show(client: &(impl Rpc + Sync), token_id: U256) -> Result<()> {
    let token_uri = fetch_token_uri(client, token_id).await?;

    println!("Token ID: {token_id:#x}");
    if let Some(json_payload) = token_uri.strip_prefix(TOKEN_URI_JSON_PREFIX) {
        match serde_json::from_str::<Value>(json_payload) {
            Ok(json) => println!("{}", serde_json::to_string_pretty(&json)?),
            Err(_) => println!("{token_uri}"),
        }
    } else {
        println!("{token_uri}");
    }

    Ok(())
}

async fn day_totals(client: &(impl Rpc + Sync), worldwide_day: WorldwideDay) -> Result<()> {
    let ret = fetch_day_totals(client, worldwide_day).await?;

    println!("WorldwideDay:           {}", worldwide_day);
    println!("Tribute Count:          {}", ret.tributeCount);
    println!("Nominal Amount Minor:   {}", ret.tributeNominalAmount);
    println!("Sealed:                 {}", ret.isSealed);
    Ok(())
}

async fn by_owner(client: &(impl Rpc + Sync), owner: Address) -> Result<()> {
    let token_ids = fetch_tributes_by_owner(client, owner).await?;

    println!("Owner: {owner:?}");
    println!("Tributes: {}", token_ids.len());
    for token_id in token_ids {
        println!("- {token_id:?}");
    }
    Ok(())
}

async fn by_day(client: &(impl Rpc + Sync), worldwide_day: WorldwideDay) -> Result<()> {
    let token_ids = fetch_tributes_by_day(client, worldwide_day).await?;

    println!("WorldwideDay: {worldwide_day}");
    println!("Tributes: {}", token_ids.len());
    for token_id in token_ids {
        println!("- {token_id:?}");
    }
    Ok(())
}

async fn supply(client: &(impl Rpc + Sync)) -> Result<()> {
    let total_supply = fetch_total_supply(client).await?;
    println!("Tribute total supply: {total_supply}");
    Ok(())
}

async fn owner(client: &(impl Rpc + Sync), token_id: U256) -> Result<()> {
    let token_owner = fetch_owner_of(client, token_id).await?;
    println!("Token ID: {token_id:#x}");
    println!("Owner:    {token_owner:?}");
    Ok(())
}

/// Submit an encrypted tribute offer. Reads the DKG-derived offer key from the
/// TeeRegistry, encrypts the payload to it (X25519 ECDHE + HKDF-SHA256 +
/// ChaCha20Poly1305, byte-identical to the enclave decrypt path), and sends
/// `offerTribute`. The enclave decrypts it inside SGX during execution and the
/// `TributeFactory` issues the canonical Tribute.
#[allow(clippy::too_many_arguments)]
async fn offer(
    client: &(impl Rpc + Sync),
    private_key: Option<&str>,
    worldwide_day: WorldwideDay,
    amount_base: String,
    amount_micro: String,
    currency: u16,
    exclude_from_intex_issuance: bool,
    zk_merkle_root: &str,
    zk_proof: &str,
    l2_chain_id: Option<u32>,
    circuit_version: Option<&str>,
    tribute_draft_id: Option<&str>,
    su_hash: Option<&str>,
    signature: &str,
) -> Result<()> {
    let signer = crate::commands::require_signer(private_key)?;
    let creator = signer.address();
    let zk_merkle_root = decode_hex_bytes(zk_merkle_root, "--zk-merkle-root")?;
    let zk_proof = decode_hex_bytes(zk_proof, "--zk-proof")?;
    let has_zk_proof = !zk_proof.is_empty();
    let (l2_chain_id, circuit_version) =
        circuit_selector(client, creator, &zk_proof, l2_chain_id, circuit_version).await?;
    let signature = decode_hex_bytes(signature, "--signature")?;
    let tribute_draft_id = offer_hex32(tribute_draft_id, "--tribute-draft-id", has_zk_proof)?;
    let su_hash = offer_hex32(su_hash, "--su-hash", has_zk_proof)?;

    // 1. Read the DKG-derived offer public key from the TeeRegistry (0xEE0A).
    let bootstrapped = {
        let r = client
            .eth_call(
                TEE_REGISTRY_ADDR,
                &ITeeRegistry::isBootstrappedCall {}.abi_encode(),
            )
            .await?;
        ITeeRegistry::isBootstrappedCall::abi_decode_returns(&r)?
    };
    if !bootstrapped {
        return Err(eyre::eyre!(
            "TeeRegistry is not bootstrapped yet - no offer key to encrypt to"
        ));
    }
    let offer_pub_u256 = {
        let r = client
            .eth_call(
                TEE_REGISTRY_ADDR,
                &ITeeRegistry::tributeOfferPublicKeyCall {}.abi_encode(),
            )
            .await?;
        ITeeRegistry::tributeOfferPublicKeyCall::abi_decode_returns(&r)?
    };
    let offer_pub: [u8; 32] = offer_pub_u256.to_be_bytes();
    println!("offer key (DKG-derived): 0x{}", hex::encode(offer_pub));

    // 2. Build the plaintext payload. The draft id + su hash must match the
    //    proof's private input; without a proof (deliberate negative
    //    transaction) they are fresh random.
    let wwd: u32 = worldwide_day.into();
    // worldwide_day + currency are cleartext ABI args (below) so the node can
    // admit and price the offer without decrypting; the ciphertext carries only
    // what must stay confidential. `referenceCurrency` is a separate axis again
    // (it drives gem/intex qualification), not the pricing key.
    let payload = serde_json::json!({
        "creator": format!("{creator:?}"),
        "tribute_draft_id": tribute_draft_id,
        "amount_base": amount_base,
        "amount_micro": amount_micro,
        "su_hashes": [su_hash],
        "wallet_addresses": [],
        "sra_addresses": [],
    });
    let plaintext = serde_json::to_vec(&payload)?;

    // 3. Encrypt to the offer key via the shared recipe (protocol
    //    `OFFER_HKDF_SALT` + ChaCha20Poly1305), byte-compatible with the enclave
    //    decrypt path and the node's canary probe.
    let (cipher_text, nonce, eph_pub) =
        outbe_tee::offer_encrypt::encrypt_tribute_offer(&offer_pub, &plaintext)
            .map_err(|e| eyre::eyre!("offer encryption failed: {e}"))?;

    // 4. Build + send `offerTribute` (msg.value MUST be 0).
    let call = ITributeFactory::offerTributeCall {
        cipherText: cipher_text.into(),
        nonce: nonce.to_vec().into(),
        ephemeralPubkey: U256::from_be_bytes(eph_pub),
        worldwideDay: wwd,
        tributeCurrency: currency,
        referenceCurrency: currency,
        excludeFromIntexIssuance: exclude_from_intex_issuance,
        zkProof: zk_proof,
        chainId: l2_chain_id,
        version: circuit_version,
        zkPublicKey: Bytes::new(),
        zkMerkleRoot: zk_merkle_root,
        signature,
    };
    // `eth_estimateGas` cannot faithfully simulate the in-enclave decrypt, so
    // send with an explicit gas limit.
    let tx_hash = signer
        .send_tx_with_gas(
            client,
            TRIBUTE_FACTORY_ADDR,
            call.abi_encode(),
            U256::ZERO,
            8_000_000,
        )
        .await?;

    println!("offerTribute tx: {tx_hash}");
    println!(
        "  creator={creator:?} worldwide_day={wwd} currency={currency} amount_base={amount_base} amount_micro={amount_micro} exclude_from_intex_issuance={exclude_from_intex_issuance}"
    );
    println!("Verify once mined: outbe-cli tribute by-owner {creator:?}");
    println!(
        "  l2_chain_id={l2_chain_id} circuit_version={:?}",
        call.version
    );
    Ok(())
}

/// Decode a `0x`-hex CLI argument into raw bytes ("" and "0x" mean empty).
fn decode_hex_bytes(value: &str, flag: &str) -> Result<Bytes> {
    let stripped = value.strip_prefix("0x").unwrap_or(value);
    if stripped.is_empty() {
        return Ok(Bytes::new());
    }
    let bytes = hex::decode(stripped).map_err(|e| eyre::eyre!("{flag} is not valid hex: {e}"))?;
    Ok(Bytes::from(bytes))
}

/// Resolve the `(chainId, version)` circuit selector carried by `offerTribute`.
///
/// Without a proof the selectors default to `0`/empty and no registry read is
/// needed. Explicit values - including ones the node will reject - are passed
/// through unchanged: whether a version is registered stays the node's check.
///
/// With a proof and no explicit version, the version is derived from the proof
/// rather than guessed. A combined proof does not name its verification key,
/// but its length is a function of that key's circuit size, so the registered
/// keys whose length matches are the only candidates. Exactly one candidate is
/// the answer; anything else is an error naming what to pass, because picking
/// the wrong key here produces an on-chain verification revert rather than a
/// message the caller can act on. Status is deliberately not a filter: the node
/// verifies `deprecated` keys too, so a proof minted under one must still be
/// offerable.
async fn circuit_selector(
    client: &(impl Rpc + Sync),
    caller: Address,
    proof: &[u8],
    l2_chain_id: Option<u32>,
    circuit_version: Option<&str>,
) -> Result<(u32, String)> {
    if proof.is_empty() {
        return Ok((
            l2_chain_id.unwrap_or(0),
            circuit_version.unwrap_or_default().to_owned(),
        ));
    }
    let chain_id = match l2_chain_id {
        Some(chain_id) => chain_id,
        None => registered_l2_chain_id(client, caller).await?,
    };
    let version = match circuit_version {
        Some(version) => version.to_owned(),
        None => {
            let keys = outbe_l2registry::api::l2_keys(
                client.eth_chain_id().await?,
                u64::from(chain_id),
                Claim::Tribute,
            );
            let candidates = keys.iter().filter_map(|key| {
                combined_len(key.vk_bytes(), PUBLIC_INPUT_COUNT)
                    .ok()
                    .map(|len| (key.version(), len))
            });
            version_for_proof_len(candidates, proof.len(), chain_id)?
        }
    };
    Ok((chain_id, version))
}

/// The one `(version, combined_len)` candidate that accepts a `proof_len`-byte
/// proof, or an error naming what the caller must pass instead.
///
/// Takes the pairs rather than the keys because `L2Key` has no constructor and
/// the compiled-in registry holds a single entry, so the ambiguous and no-match
/// arms are unreachable through it; the pairs are what the rule reads off a key.
fn version_for_proof_len<'a>(
    candidates: impl Iterator<Item = (&'a str, usize)>,
    proof_len: usize,
    chain_id: u32,
) -> Result<String> {
    let matching: Vec<&str> = candidates
        .filter(|(_, len)| *len == proof_len)
        .map(|(version, _)| version)
        .collect();
    match matching.as_slice() {
        [version] => Ok((*version).to_owned()),
        [] => Err(eyre::eyre!(
            "no tribute circuit registered for L2 chain {chain_id} takes a \
             {proof_len}-byte proof; pass --circuit-version"
        )),
        several => Err(eyre::eyre!(
            "a {proof_len}-byte proof fits several tribute circuits registered \
             for L2 chain {chain_id} ({}); pass --circuit-version",
            several.join(", ")
        )),
    }
}

/// The caller's registered L2 chain id, read from the L2Registry (0xEE0E).
async fn registered_l2_chain_id(client: &(impl Rpc + Sync), caller: Address) -> Result<u32> {
    let call = IL2Registry::chainIdByL1AddressCall { l1Address: caller };
    let result = client
        .eth_call(L2_REGISTRY_ADDRESS, &call.abi_encode())
        .await?;
    let chain_id = IL2Registry::chainIdByL1AddressCall::abi_decode_returns(&result)?;
    if chain_id == 0 {
        return Err(eyre::eyre!(
            "caller {caller:?} is not a registered L2 operator"
        ));
    }
    u32::try_from(chain_id).map_err(|_| {
        eyre::eyre!("registered L2 chain id {chain_id} does not fit the uint32 circuit selector")
    })
}

/// 32 fresh random bytes as a `0x`-hex string (offer draft id / su hash).
fn random_hex32() -> Result<String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| eyre::eyre!("rng failure"))?;
    Ok(format!("0x{}", hex::encode(bytes)))
}

fn offer_hex32(value: Option<&str>, flag: &str, required: bool) -> Result<String> {
    let Some(value) = value else {
        if required {
            return Err(eyre::eyre!("{flag} is required with --zk-proof"));
        }
        return random_hex32();
    };
    let bytes = decode_hex_bytes(value, flag)?;
    if bytes.len() != 32 {
        return Err(eyre::eyre!(
            "{flag} must contain exactly 32 bytes, got {}",
            bytes.len()
        ));
    }
    Ok(format!("0x{}", hex::encode(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::mock::{abi_u64, call_map, MockRpc};
    use alloy_primitives::address;
    use clap::Parser;
    use std::collections::HashMap;

    #[derive(Parser)]
    struct TributeHarness {
        #[command(subcommand)]
        command: TributeCmd,
    }

    #[derive(serde::Deserialize)]
    struct CanonicalBaseCases {
        accepted_base: Vec<String>,
        rejected_base: Vec<String>,
        accepted_micro: Vec<String>,
        rejected_micro: Vec<String>,
    }

    fn canonical_base_cases() -> CanonicalBaseCases {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../testdata/tribute/canonical-amounts-v1.json"
        )))
        .unwrap()
    }

    fn sample_token_id() -> U256 {
        U256::from(0xaa_u64)
    }

    fn tribute_mock() -> MockRpc {
        let owner = address!("0x1111111111111111111111111111111111111111");
        let token_uri = "data:application/json;utf8,{\"name\":\"Tribute 170\",\"attributes\":[{\"trait_type\":\"worldwide_day\",\"value\":20241220}]}".to_string();
        let day_totals = (2u32, U256::from(500u64), true).into();
        let token_ids = vec![sample_token_id()];

        let mut map = HashMap::new();
        map.insert(
            (TRIBUTE_ADDR, ITribute::totalSupplyCall::SELECTOR),
            ITribute::totalSupplyCall::abi_encode_returns(&U256::from(3u64)),
        );
        map.insert(
            (TRIBUTE_ADDR, ITribute::ownerOfCall::SELECTOR),
            ITribute::ownerOfCall::abi_encode_returns(&owner),
        );
        map.insert(
            (TRIBUTE_ADDR, ITribute::tokenURICall::SELECTOR),
            ITribute::tokenURICall::abi_encode_returns(&token_uri),
        );
        map.insert(
            (TRIBUTE_ADDR, ITribute::getDayTotalsCall::SELECTOR),
            ITribute::getDayTotalsCall::abi_encode_returns(&day_totals),
        );
        map.insert(
            (TRIBUTE_ADDR, ITribute::getTributesByOwnerCall::SELECTOR),
            ITribute::getTributesByOwnerCall::abi_encode_returns(&token_ids),
        );
        map.insert(
            (TRIBUTE_ADDR, ITribute::getTributesByDayCall::SELECTOR),
            ITribute::getTributesByDayCall::abi_encode_returns(&token_ids),
        );

        MockRpc {
            eth_call_map: Some(call_map(map)),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_fetch_token_uri_returns_metadata_json() {
        let mock = tribute_mock();

        let result = fetch_token_uri(&mock, sample_token_id()).await.unwrap();
        assert!(result.starts_with(TOKEN_URI_JSON_PREFIX));
        assert!(result.contains("worldwide_day"));
    }

    #[tokio::test]
    async fn test_fetch_day_totals_returns_expected_values() {
        let mock = tribute_mock();

        let result = fetch_day_totals(&mock, 20241220u32.into()).await.unwrap();
        assert_eq!(result.tributeCount, 2);
        assert_eq!(result.tributeNominalAmount, U256::from(500u64));
        assert!(result.isSealed);
    }

    #[tokio::test]
    async fn test_fetch_tributes_by_owner_returns_token_ids() {
        let owner = address!("0x1111111111111111111111111111111111111111");
        let mock = tribute_mock();

        let result = fetch_tributes_by_owner(&mock, owner).await.unwrap();
        assert_eq!(result, vec![sample_token_id()]);
    }

    #[tokio::test]
    async fn test_show_uses_token_uri_without_error() {
        let mock = tribute_mock();

        show(&mock, sample_token_id()).await.unwrap();
    }

    #[tokio::test]
    async fn test_supply_returns_without_error() {
        let mock = tribute_mock();
        supply(&mock).await.unwrap();
    }

    #[test]
    fn zk_offer_requires_explicit_draft_inputs() {
        let error = offer_hex32(None, "--tribute-draft-id", true).unwrap_err();
        assert!(error
            .to_string()
            .contains("--tribute-draft-id is required with --zk-proof"));
    }

    /// L2Registry mock answering `chainIdByL1Address` for the offer caller.
    fn l2_registry_mock(chain_id: u64) -> MockRpc {
        let mut map = HashMap::new();
        map.insert(
            (
                L2_REGISTRY_ADDRESS,
                IL2Registry::chainIdByL1AddressCall::SELECTOR,
            ),
            abi_u64(chain_id),
        );
        MockRpc {
            chain_id: Ok(outbe_primitives::chain::DEVNET_CHAIN_ID),
            eth_call_map: Some(call_map(map)),
            ..Default::default()
        }
    }

    /// A stand-in proof of exactly the length the registered key implies, so
    /// the selector derives a version from it the way a real one would.
    fn sized_proof() -> Vec<u8> {
        let vk = outbe_l2registry::api::vk_for(
            outbe_primitives::chain::DEVNET_CHAIN_ID,
            0xdead,
            Claim::Tribute,
            "1.0.0",
        )
        .expect("the test L2 registers the fixture circuit version");
        vec![0u8; combined_len(vk, PUBLIC_INPUT_COUNT).expect("combined length under the key")]
    }

    #[tokio::test]
    async fn offer_without_a_proof_sends_the_empty_circuit_selector() {
        let caller = address!("0x1111111111111111111111111111111111111111");
        // Non-ZK selectors default independently without any registry RPC.
        for (chain_id, version, expected) in [
            (None, None, (0, "")),
            (Some(7), None, (7, "")),
            (None, Some("custom"), (0, "custom")),
        ] {
            let selector = circuit_selector(&MockRpc::default(), caller, &[], chain_id, version)
                .await
                .unwrap();
            assert_eq!(selector, (expected.0, expected.1.to_owned()));
        }
    }

    #[tokio::test]
    async fn proof_defaults_to_the_caller_registered_l2_and_its_canonical_version() {
        let caller = address!("0x1111111111111111111111111111111111111111");
        let registered = l2_registry_mock(0xdead);
        let selector = circuit_selector(&registered, caller, &sized_proof(), None, None)
            .await
            .unwrap();
        assert_eq!(selector, (0xdead, "1.0.0".to_owned()));
    }

    #[tokio::test]
    async fn explicit_circuit_selectors_are_passed_through_unchanged() {
        let caller = address!("0x1111111111111111111111111111111111111111");
        // A default mock fails every eth_call: explicit selectors need no RPC.
        let offline = MockRpc::default();
        assert_eq!(
            circuit_selector(&offline, caller, &sized_proof(), Some(7), Some("9.9.9"))
                .await
                .unwrap(),
            (7, "9.9.9".to_owned())
        );
        // An explicit empty version must not be replaced by the default.
        assert_eq!(
            circuit_selector(&offline, caller, &sized_proof(), Some(7), Some(""))
                .await
                .unwrap(),
            (7, String::new())
        );
        // Each selector defaults independently of the other.
        let registered = l2_registry_mock(0xdead);
        assert_eq!(
            circuit_selector(&registered, caller, &sized_proof(), None, Some("1.0.0"))
                .await
                .unwrap(),
            (0xdead, "1.0.0".to_owned())
        );
        assert_eq!(
            circuit_selector(&registered, caller, &sized_proof(), Some(0xdead), None)
                .await
                .unwrap(),
            (0xdead, "1.0.0".to_owned())
        );
    }

    #[tokio::test]
    async fn proof_without_explicit_selectors_requires_a_registered_canonical_circuit() {
        let caller = address!("0x1111111111111111111111111111111111111111");
        let unregistered = l2_registry_mock(0);
        assert!(
            circuit_selector(&unregistered, caller, &sized_proof(), None, None)
                .await
                .is_err(),
            "an unregistered caller must not fall back to chain 0"
        );
        let oversize = l2_registry_mock(u64::from(u32::MAX) + 1);
        assert!(
            circuit_selector(&oversize, caller, &sized_proof(), None, None)
                .await
                .is_err(),
            "a chain id wider than uint32 must not be truncated"
        );
        let non_development = MockRpc {
            chain_id: Ok(outbe_primitives::chain::MAINNET_CHAIN_ID),
            ..Default::default()
        };
        assert!(
            circuit_selector(&non_development, caller, &sized_proof(), Some(999), None)
                .await
                .is_err(),
            "a chain with no registered tribute circuit has no default version"
        );
    }

    #[test]
    fn explicit_offer_input_must_be_exactly_32_bytes() {
        let error = offer_hex32(Some("0x0102"), "--su-hash", true).unwrap_err();
        assert!(error
            .to_string()
            .contains("--su-hash must contain exactly 32 bytes"));

        let value = format!("0x{}", "01".repeat(32));
        assert_eq!(offer_hex32(Some(&value), "--su-hash", true).unwrap(), value);
    }

    /// Every required offer flag, in `flag, value` pairs.
    const REQUIRED_OFFER_FLAGS: [&str; 6] = [
        "--zk-proof",
        "0x",
        "--zk-merkle-root",
        "0x",
        "--signature",
        "0x",
    ];

    /// `tribute offer 20250115` plus `extra` plus the required ZK flags.
    fn offer_argv(extra: &[&str]) -> Vec<String> {
        let mut argv: Vec<String> = Vec::new();
        argv.extend(["tribute", "offer", "20250115"].map(str::to_owned));
        argv.extend(extra.iter().map(|value| (*value).to_owned()));
        argv.extend(REQUIRED_OFFER_FLAGS.iter().map(|value| (*value).to_owned()));
        argv
    }

    #[test]
    fn offer_cli_requires_the_zk_offer_inputs() {
        for missing in ["--zk-proof", "--zk-merkle-root", "--signature"] {
            let mut argv = offer_argv(&[]);
            let index = argv
                .iter()
                .position(|arg| arg == missing)
                .unwrap_or_else(|| panic!("{missing} is not in the offered argv"));
            argv.drain(index..=index + 1);
            assert!(
                TributeHarness::try_parse_from(argv).is_err(),
                "{missing} must be required"
            );
        }
        assert!(TributeHarness::try_parse_from(offer_argv(&[])).is_ok());
    }

    #[test]
    fn offer_cli_accepts_only_canonical_unsigned_whole_base_amounts() {
        let cases = canonical_base_cases();
        for canonical in cases.accepted_base {
            assert!(TributeHarness::try_parse_from(offer_argv(&["--amount", &canonical])).is_ok());
        }

        for noncanonical in cases.rejected_base {
            assert!(
                TributeHarness::try_parse_from(offer_argv(&["--amount", &noncanonical])).is_err(),
                "non-canonical amount_base {noncanonical:?} was accepted"
            );
        }
    }

    #[test]
    fn offer_cli_accepts_only_canonical_six_decimal_micro_remainders() {
        let cases = canonical_base_cases();
        for canonical in cases.accepted_micro {
            assert!(
                TributeHarness::try_parse_from(offer_argv(&["--amount-micro", &canonical])).is_ok()
            );
        }

        for noncanonical in cases.rejected_micro {
            assert!(
                TributeHarness::try_parse_from(offer_argv(&["--amount-micro", &noncanonical]))
                    .is_err(),
                "non-canonical amount_micro {noncanonical:?} was accepted"
            );
        }
    }

    #[test]
    fn offer_cli_rejects_legacy_amount_atto_flag() {
        assert!(TributeHarness::try_parse_from(offer_argv(&["--amount-atto", "0"])).is_err());
    }

    const LEN: usize = 8_900;

    /// One candidate of the right length is the answer, whatever its status:
    /// the node verifies `deprecated` keys, so a proof minted under one must
    /// still be offerable without naming it.
    #[test]
    fn one_candidate_of_the_right_length_is_the_answer() {
        let picked = version_for_proof_len([("1.0.0", LEN)].into_iter(), LEN, 0xdead).unwrap();
        assert_eq!(picked, "1.0.0");
        let picked = version_for_proof_len(
            [("1.0.0", LEN), ("2.0.0", LEN + 384)].into_iter(),
            LEN,
            0xdead,
        )
        .unwrap();
        assert_eq!(picked, "1.0.0");
    }

    /// Two circuits of the same size cannot be told apart from the proof, and
    /// guessing would revert on chain instead of here. Both are named.
    #[test]
    fn an_ambiguous_length_is_an_error_naming_the_candidates() {
        let error =
            version_for_proof_len([("1.0.0", LEN), ("2.0.0", LEN)].into_iter(), LEN, 0xdead)
                .expect_err("an ambiguous proof length must not be guessed");
        let message = error.to_string();
        assert!(
            message.contains("1.0.0") && message.contains("2.0.0"),
            "{message}"
        );
        assert!(message.contains("--circuit-version"), "{message}");
    }

    #[test]
    fn no_candidate_of_that_length_is_an_error() {
        let error = version_for_proof_len([("1.0.0", LEN)].into_iter(), LEN + 32, 0xdead)
            .expect_err("a proof no registered key accepts must be rejected");
        assert!(error.to_string().contains("--circuit-version"));
    }

    #[test]
    fn an_empty_registry_is_an_error() {
        assert!(version_for_proof_len([].into_iter(), LEN, 0xdead).is_err());
    }
}
