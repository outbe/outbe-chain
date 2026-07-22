//! Tribute commands.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use clap::Subcommand;
use eyre::Result;
use outbe_common::WorldwideDay;
use serde_json::Value;

use crate::abi::{
    ITeeRegistry, ITribute, ITributeFactory, TEE_REGISTRY_ADDR, TRIBUTE_ADDR, TRIBUTE_FACTORY_ADDR,
};
use crate::rpc::Rpc;

const TOKEN_URI_JSON_PREFIX: &str = "data:application/json;utf8,";

type DayTotalsReturn = <ITribute::getDayTotalsCall as SolCall>::Return;
type TokenIdsReturn = <ITribute::getTributesByOwnerCall as SolCall>::Return;

#[derive(Subcommand)]
pub enum TributeCmd {
    /// Show tribute metadata via tokenURI JSON
    Show {
        /// Tribute token ID
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
        /// Tribute token ID
        token_id: U256,
    },
    /// Submit an encrypted tribute offer (decrypted inside the SGX enclave).
    /// Encrypts to the DKG-derived offer key registered in the TeeRegistry and
    /// sends `offerTribute`; requires `--private-key`.
    Offer {
        /// WorldwideDay (must be in OFFERING status), e.g. 20241220
        worldwide_day: WorldwideDay,
        /// Issuance amount in whole units (`amount_base`)
        #[arg(long, default_value = "100")]
        amount: String,
        /// ISO 4217 currency code (840 = USD)
        #[arg(long, default_value_t = 840)]
        currency: u16,
        /// Exclude the resulting Tribute from Intex issuance
        #[arg(long, default_value_t = false)]
        exclude_from_intex_issuance: bool,
        /// L2 zkMerkleRoot bytes (`0x`-hex). Required together with
        /// `--signature` when the sender is a registered L2 operator whose
        /// network has ZK verification enabled in the L2Registry.
        #[arg(long, default_value = "0x")]
        zk_merkle_root: String,
        /// BLS MinPk signature (96 bytes, `0x`-hex) over `--zk-merkle-root`
        /// produced with the network key registered in the L2Registry.
        #[arg(long, default_value = "0x")]
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
                currency,
                exclude_from_intex_issuance,
                zk_merkle_root,
                signature,
            } => {
                offer(
                    client,
                    private_key,
                    worldwide_day,
                    amount,
                    currency,
                    exclude_from_intex_issuance,
                    &zk_merkle_root,
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
    let call = ITribute::ownerOfCall { tokenId: token_id };
    let result = client.eth_call(TRIBUTE_ADDR, &call.abi_encode()).await?;
    Ok(ITribute::ownerOfCall::abi_decode_returns(&result)?)
}

async fn fetch_token_uri(client: &(impl Rpc + Sync), token_id: U256) -> Result<String> {
    let call = ITribute::tokenURICall { tokenId: token_id };
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

    println!("Token ID: {token_id:?}");
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
    println!("Total Gratis Load:      {}", ret.totalGratisLoadMinor);
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
    println!("Token ID: {token_id:?}");
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
    currency: u16,
    exclude_from_intex_issuance: bool,
    zk_merkle_root: &str,
    signature: &str,
) -> Result<()> {
    let signer = crate::commands::require_signer(private_key)?;
    let creator = signer.address();
    let zk_merkle_root = decode_hex_bytes(zk_merkle_root, "--zk-merkle-root")?;
    let signature = decode_hex_bytes(signature, "--signature")?;

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
            "TeeRegistry is not bootstrapped yet — no offer key to encrypt to"
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

    // 2. Build the plaintext payload. `tribute_draft_id` + `su_hashes` are fresh
    //    random — su hashes must be unique per offer.
    let wwd: u32 = worldwide_day.into();
    // worldwide_day + currency are the authoritative offer fields (encrypted);
    // they also travel cleartext as ABI args so the node can resolve the price,
    // and the enclave verifies the two copies match.
    let payload = serde_json::json!({
        "creator": format!("{creator:?}"),
        "tribute_draft_id": random_hex32()?,
        "worldwide_day": wwd,
        "currency": currency,
        "amount_base": amount_base,
        "amount_atto": "0",
        "su_hashes": [random_hex32()?],
        "wallet_addresses": [],
        "sra_addresses": [],
    });
    let plaintext = serde_json::to_vec(&payload)?;

    // 3. Encrypt to the offer key. The HKDF salt is the protocol constant
    //    `OFFER_HKDF_SALT` shared with the enclave decrypt path — NOT the legacy
    //    `[0x03; 32]`, which silently produces a different AEAD key (offer rejected
    //    `AEAD decryption failed`).
    let salt = outbe_tee::OFFER_HKDF_SALT;
    let (cipher_text, nonce, eph_pub) = encrypt_offer(&offer_pub, &salt, &plaintext)?;

    // 4. Build + send `offerTribute` (msg.value MUST be 0; ZK fields are stubs).
    let call = ITributeFactory::offerTributeCall {
        cipherText: cipher_text.into(),
        nonce: nonce.to_vec().into(),
        ephemeralPubkey: U256::from_be_bytes(eph_pub),
        referenceCurrency: currency,
        excludeFromIntexIssuance: exclude_from_intex_issuance,
        zkProof: Bytes::new(),
        zkVerificationKey: Bytes::new(),
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
        "  creator={creator:?} worldwide_day={wwd} currency={currency} amount_base={amount_base} exclude_from_intex_issuance={exclude_from_intex_issuance}"
    );
    println!("Verify once mined: outbe-cli tribute by-owner {creator:?}");
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

/// 32 fresh random bytes as a `0x`-hex string (offer draft id / su hash).
fn random_hex32() -> Result<String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| eyre::eyre!("rng failure"))?;
    Ok(format!("0x{}", hex::encode(bytes)))
}

/// Encrypt an offer payload to `offer_pub`: ephemeral X25519 ECDHE → HKDF-SHA256
/// → ChaCha20Poly1305 (empty AAD). Byte-identical to the enclave decrypt path
/// (`outbe_tee_enclave::crypto::ecdhe_offer_decrypt`). Returns
/// `(cipher_text, nonce, ephemeral_pub)`.
fn encrypt_offer(
    offer_pub: &[u8; 32],
    salt: &[u8; 32],
    plaintext: &[u8],
) -> Result<(Vec<u8>, [u8; 12], [u8; 32])> {
    use ring::rand::SecureRandom;
    use x25519_dalek::{PublicKey, StaticSecret};

    let rng = ring::rand::SystemRandom::new();
    let mut eph_bytes = [0u8; 32];
    rng.fill(&mut eph_bytes)
        .map_err(|_| eyre::eyre!("rng failure"))?;
    let eph_secret = StaticSecret::from(eph_bytes);
    let eph_pub = PublicKey::from(&eph_secret).to_bytes();

    let shared = eph_secret.diffie_hellman(&PublicKey::from(*offer_pub));
    let key = hkdf_sha256(salt, shared.as_bytes(), b"tribute-factory-encryption")?;

    let mut nonce = [0u8; 12];
    rng.fill(&mut nonce)
        .map_err(|_| eyre::eyre!("rng failure"))?;
    let cipher_text = chacha20poly1305_encrypt(&key, &nonce, plaintext)?;
    Ok((cipher_text, nonce, eph_pub))
}

/// HKDF-SHA256 extract+expand to 32 bytes (matches the enclave).
fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8]) -> Result<[u8; 32]> {
    use ring::hkdf;
    struct Len32;
    impl hkdf::KeyType for Len32 {
        fn len(&self) -> usize {
            32
        }
    }
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, salt);
    let prk = salt.extract(ikm);
    // Bind to a `let` so the `[info]` array outlives the `Okm` that borrows it.
    let info_refs: &[&[u8]] = &[info];
    let okm = prk
        .expand(info_refs, Len32)
        .map_err(|_| eyre::eyre!("hkdf expand"))?;
    let mut out = [0u8; 32];
    okm.fill(&mut out).map_err(|_| eyre::eyre!("hkdf fill"))?;
    Ok(out)
}

/// ChaCha20Poly1305 AEAD encrypt with empty AAD (matches the enclave).
fn chacha20poly1305_encrypt(key: &[u8; 32], nonce: &[u8; 12], plaintext: &[u8]) -> Result<Vec<u8>> {
    use ring::aead::{self, BoundKey as _};
    struct OneNonce([u8; 12]);
    impl aead::NonceSequence for OneNonce {
        fn advance(&mut self) -> std::result::Result<aead::Nonce, ring::error::Unspecified> {
            Ok(aead::Nonce::assume_unique_for_key(self.0))
        }
    }
    let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key)
        .map_err(|_| eyre::eyre!("aead key"))?;
    let mut sealing = aead::SealingKey::new(unbound, OneNonce(*nonce));
    let mut in_out = plaintext.to_vec();
    sealing
        .seal_in_place_append_tag(aead::Aad::empty(), &mut in_out)
        .map_err(|_| eyre::eyre!("aead seal"))?;
    Ok(in_out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::mock::{call_map, MockRpc};
    use alloy_primitives::address;
    use std::collections::HashMap;

    fn tribute_mock() -> MockRpc {
        let token_id = U256::from(0xaau64);
        let owner = address!("0x1111111111111111111111111111111111111111");
        let token_uri = "data:application/json;utf8,{\"name\":\"Tribute 170\",\"attributes\":[{\"trait_type\":\"worldwide_day\",\"value\":20241220}]}".to_string();
        let day_totals = (2u32, U256::from(500u64), U256::from(75u64), true).into();
        let token_ids = vec![token_id];

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
        let token_id = U256::from(0xaau64);
        let mock = tribute_mock();

        let result = fetch_token_uri(&mock, token_id).await.unwrap();
        assert!(result.starts_with(TOKEN_URI_JSON_PREFIX));
        assert!(result.contains("worldwide_day"));
    }

    #[tokio::test]
    async fn test_fetch_day_totals_returns_expected_values() {
        let mock = tribute_mock();

        let result = fetch_day_totals(&mock, 20241220u32.into()).await.unwrap();
        assert_eq!(result.tributeCount, 2);
        assert_eq!(result.tributeNominalAmount, U256::from(500u64));
        assert_eq!(result.totalGratisLoadMinor, U256::from(75u64));
        assert!(result.isSealed);
    }

    #[tokio::test]
    async fn test_fetch_tributes_by_owner_returns_token_ids() {
        let owner = address!("0x1111111111111111111111111111111111111111");
        let expected = U256::from(0xaau64);
        let mock = tribute_mock();

        let result = fetch_tributes_by_owner(&mock, owner).await.unwrap();
        assert_eq!(result, vec![expected]);
    }

    #[tokio::test]
    async fn test_show_uses_token_uri_without_error() {
        let token_id = U256::from(0xaau64);
        let mock = tribute_mock();

        show(&mock, token_id).await.unwrap();
    }

    #[tokio::test]
    async fn test_supply_returns_without_error() {
        let mock = tribute_mock();
        supply(&mock).await.unwrap();
    }
}
