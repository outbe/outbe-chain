//! `issueNod`: the offer of a Worldwide Day issues its Nod in the same transaction.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolError;
use outbe_agentreward::AgentRewardContract;
use outbe_core_common::nod_encryption::EncryptedNodV1;
use outbe_core_common::series_id::SeriesId;
use outbe_nod::api as nod_api;
use outbe_nod::offer::issue::{IssueNodAtOfferRequestV1, NodAtOfferResultV1, NodAtOfferStatusV1};
use outbe_nod::offer::protocol::{
    EncryptedOffer, OfferPublicInputs, OfferZkContext, OfferZkExpectedHashes,
};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::stablecoin::validate_currency_code;
use outbe_system_common::time::{timestamp_to_date_key, WorldwideDay};
use outbe_zk::wire::{decode_public_words, read_u64_be_padded};
use outbe_zk_backend::barretenberg::{Barretenberg, RawVerifier};
use thiserror::Error;

use crate::precompile::INodFactory;

const SECONDS_PER_HOUR: u64 = 3600;

/// The public inputs of `issueNod`. `caller` is bound to the proof.
pub struct IssueNodInput {
    pub caller: Address,
    pub cipher_text: Bytes,
    pub nonce: Bytes,
    pub ephemeral_pubkey: U256,
    pub worldwide_day: WorldwideDay,
    pub issuance_currency: u16,
    pub reference_currency: u16,
    pub zk_proof: Bytes,
    pub l2_chain_id: u32,
    pub circuit_version: String,
    pub zk_merkle_root: Bytes,
    pub signature: Bytes,
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OfferError {
    #[error("enclave rejected the offer: {0}")]
    EnclaveRejected(String),
    #[error("offer enclave: {0}")]
    Enclave(#[source] outbe_tee::TransportError),
    #[error("tee_enclave_nondeterminism: unexpected offer response")]
    UnexpectedResponse,
    #[error("the Nod load is below one minor unit")]
    LoadBelowOneMinorUnit,
    #[error("zkProof is required")]
    ZkProofRequired,
    #[error("unknown circuit version {version:?} for L2 chain {chain_id}")]
    UnknownCircuitVersion { chain_id: u32, version: String },
    #[error("ZK circuit does not support the offer proof interface")]
    UnsupportedZkCircuit,
    #[error("malformed zkProof: {0}")]
    MalformedZkProof(String),
    #[error("ZK public input mismatch: {field}")]
    ZkPublicInputMismatch { field: &'static str },
    #[error("ZK proof verification failed")]
    InvalidZkProof,
    #[error("ZK verifier unavailable: zk verification backend failed: {0}")]
    ZkVerifierUnavailable(String),
    #[error("worldwide_day {worldwide_day} is not a valid YYYYMMDD calendar date")]
    InvalidWorldwideDay { worldwide_day: WorldwideDay },
    #[error("worldwide_day {worldwide_day} is not in OFFERING status")]
    WorldwideDayNotOffering { worldwide_day: WorldwideDay },
    #[error("issuance currency {issuance_currency} not registered in oracle")]
    IssuanceCurrencyNotRegistered { issuance_currency: u16 },
    #[error("no 24-hour VWAP at the offer hour for worldwide_day {worldwide_day}")]
    CurrencyPriceUnavailable { worldwide_day: WorldwideDay },
    #[error("SU hash list is empty")]
    EmptySuHashList,
    #[error("invalid SU hash hex: {hash}")]
    InvalidSuHashHex { hash: String },
    #[error("SU hash must be 32 bytes, got {length}")]
    InvalidSuHashLength { length: usize },
    #[error("wallet_addresses must be specified when sra_addresses is provided")]
    WalletAddressesRequiredWhenSraProvided,
    #[error("sra_addresses must be specified when wallet_addresses is provided")]
    SraAddressesRequiredWhenWalletProvided,
    #[error("invalid wallet_address at index {index}: {address}")]
    InvalidWalletAddressAtIndex { index: usize, address: String },
    #[error("invalid sra_address at index {index}: {address}")]
    InvalidSraAddressAtIndex { index: usize, address: String },
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

fn index_u64(index: usize) -> u64 {
    u64::try_from(index).unwrap_or(u64::MAX)
}

impl From<OfferError> for PrecompileError {
    fn from(value: OfferError) -> Self {
        use INodFactory as I;
        match value {
            OfferError::Enclave(error) if error.is_node_local() => {
                PrecompileError::EnclaveUnavailable(error.to_string())
            }
            OfferError::UnexpectedResponse => {
                PrecompileError::EnclaveUnavailable(value.to_string())
            }
            OfferError::Enclave(error) => revert_with(I::OfferEnclaveFailed {
                reason: error.to_string(),
            }),
            OfferError::EnclaveRejected(reason) => revert_with(I::EnclaveRejected { reason }),
            OfferError::LoadBelowOneMinorUnit => revert_with(I::NodLoadBelowOneMinorUnit {}),
            OfferError::ZkProofRequired => revert_with(I::ZkProofRequired {}),
            OfferError::UnknownCircuitVersion { chain_id, version } => {
                revert_with(I::UnknownCircuitVersion {
                    chainId: chain_id,
                    version,
                })
            }
            OfferError::UnsupportedZkCircuit => revert_with(I::UnsupportedZkCircuit {}),
            OfferError::MalformedZkProof(value) => revert_with(I::MalformedZkProof { value }),
            OfferError::ZkPublicInputMismatch { field } => revert_with(I::ZkPublicInputMismatch {
                field: field.to_string(),
            }),
            OfferError::InvalidZkProof => revert_with(I::InvalidZkProof {}),
            OfferError::ZkVerifierUnavailable(value) => {
                revert_with(I::ZkVerifierUnavailable { value })
            }
            OfferError::InvalidWorldwideDay { worldwide_day } => {
                revert_with(I::InvalidWorldwideDay {
                    worldwideDay: worldwide_day.value(),
                })
            }
            OfferError::WorldwideDayNotOffering { worldwide_day } => {
                revert_with(I::WorldwideDayNotOffering {
                    worldwideDay: worldwide_day.value(),
                })
            }
            OfferError::IssuanceCurrencyNotRegistered { issuance_currency } => {
                revert_with(I::IssuanceCurrencyNotRegistered {
                    issuanceCurrency: issuance_currency,
                })
            }
            OfferError::CurrencyPriceUnavailable { worldwide_day } => {
                revert_with(I::CurrencyPriceUnavailable {
                    worldwideDay: worldwide_day.value(),
                })
            }
            OfferError::EmptySuHashList => revert_with(I::EmptySuHashList {}),
            OfferError::InvalidSuHashHex { hash } => revert_with(I::InvalidSuHashHex { hash }),
            OfferError::InvalidSuHashLength { length } => revert_with(I::InvalidSuHashLength {
                length: index_u64(length),
            }),
            OfferError::WalletAddressesRequiredWhenSraProvided => {
                revert_with(I::WalletAddressesRequiredWhenSraProvided {})
            }
            OfferError::SraAddressesRequiredWhenWalletProvided => {
                revert_with(I::SraAddressesRequiredWhenWalletProvided {})
            }
            OfferError::InvalidWalletAddressAtIndex { index, address } => {
                revert_with(I::InvalidWalletAddressAtIndex {
                    index: index_u64(index),
                    walletAddress: address,
                })
            }
            OfferError::InvalidSraAddressAtIndex { index, address } => {
                revert_with(I::InvalidSraAddressAtIndex {
                    index: index_u64(index),
                    sraAddress: address,
                })
            }
        }
    }
}

struct VerifiedProof {
    host_chain_id: u64,
    public: OfferPublicInputs,
    verification_key: &'static [u8],
}

/// The claims of an offer that the enclave opened, parsed.
pub(crate) struct OfferClaims {
    su_hashes: Vec<B256>,
    wallets: Vec<Address>,
    sras: Vec<Address>,
}

/// The offer of `input` issues its Nod. Every check and every write runs in this call.
pub fn issue_nod(storage: &StorageHandle<'_>, input: IssueNodInput) -> Result<U256> {
    validate_currency_code(input.issuance_currency)?;
    validate_currency_code(input.reference_currency)?;
    let proof = verified_proof(storage, &input)?;
    let (wwd, pricing) = pricing(storage, &input)?;
    let now = storage.now_secs()?;
    let (sub_series_id, entry_price_minor) = offer_terms(storage, &wwd, &input, now, &pricing)?;
    let request = offer_request(
        &input,
        (wwd.day, entry_price_minor),
        &pricing,
        OfferZkContext {
            derived_owner: proof.public.derived_owner,
            chain_id: proof.host_chain_id,
            l2_chain_id: u64::from(input.l2_chain_id),
        },
    );
    let (nod, result) = issued_nod(nod_api::issue_at_offer(storage, request)?)?;
    validate_zk_result(
        &input.zk_proof,
        proof.verification_key,
        proof.public,
        result.zk_expected_hashes.as_ref(),
    )?;
    let claims = offer_claims(&result)?;
    record_offer(
        storage,
        input.worldwide_day,
        &nod,
        (now, sub_series_id),
        &claims,
    )
}

/// The sub-series of the offer hour of `now` and its entry price: the price of its first
/// Nod, or the 24-hour reference VWAP of the hour for a new sub-series.
pub(crate) fn offer_terms(
    storage: &StorageHandle<'_>,
    wwd: &outbe_worldwideday::WwdProjection,
    input: &IssueNodInput,
    now: u64,
    pricing: &outbe_oracle::api::TributePricingInputs,
) -> Result<(u16, U256)> {
    let sub_series_id = offer_hour(now, wwd.offerings_start)?;
    let series_id = SeriesId::for_pair(wwd.day, input.issuance_currency, input.reference_currency)
        .map_err(|_| crate::errors::NodFactoryError::InvalidEncryptedNod)?;
    let entry_price_minor = nod_api::sub_series_entry_price(storage, series_id, sub_series_id)?
        .unwrap_or(pricing.reference_vwap_minor);
    Ok((sub_series_id, entry_price_minor))
}

pub(crate) fn offer_request(
    input: &IssueNodInput,
    (emission_day, entry_price_minor): (u32, U256),
    pricing: &outbe_oracle::api::TributePricingInputs,
    zk_context: OfferZkContext,
) -> IssueNodAtOfferRequestV1 {
    IssueNodAtOfferRequestV1 {
        chain_id: zk_context.chain_id,
        offer: EncryptedOffer {
            owner: input.caller,
            cipher_text: input.cipher_text.to_vec(),
            nonce: input.nonce.to_vec(),
            ephemeral_pubkey: input.ephemeral_pubkey,
            worldwide_day: input.worldwide_day,
            tribute_currency: input.issuance_currency,
            reference_currency: input.reference_currency,
            issuance_wwd_vwap_minor: pricing.issuance_vwap_minor,
            reference_wwd_vwap_minor: pricing.reference_vwap_minor,
            reference_scurve_minor: pricing.reference_scurve_minor,
            zk_context: Some(zk_context),
        },
        emission_day,
        entry_price_minor,
    }
}

/// The Nod of an issued offer. A refused offer reverts with its cause.
pub(crate) fn issued_nod(
    response: std::result::Result<NodAtOfferResultV1, outbe_tee::TransportError>,
) -> Result<(EncryptedNodV1, NodAtOfferResultV1)> {
    let result = response.map_err(OfferError::Enclave)?;
    match &result.status {
        NodAtOfferStatusV1::Issued => {}
        NodAtOfferStatusV1::LoadBelowOneMinorUnit => {
            return Err(OfferError::LoadBelowOneMinorUnit.into())
        }
        NodAtOfferStatusV1::Rejected { reason } => {
            return Err(OfferError::EnclaveRejected(reason.clone()).into())
        }
    }
    let nod = result.nod.clone().ok_or(OfferError::UnexpectedResponse)?;
    if result.su_hashes.is_empty() {
        return Err(OfferError::EmptySuHashList.into());
    }
    Ok((nod, result))
}

pub(crate) fn offer_claims(result: &NodAtOfferResultV1) -> Result<OfferClaims> {
    let su_hashes = parse_su_hashes(&result.su_hashes)?;
    let (wallets, sras) =
        validate_agent_reward_addresses(&result.wallet_addresses, &result.sra_addresses)?;
    Ok(OfferClaims {
        su_hashes,
        wallets,
        sras,
    })
}

/// Every write of an offer whose proof holds: its marks, its Nod, the day sums and the
/// agent reward activity.
pub(crate) fn record_offer(
    storage: &StorageHandle<'_>,
    worldwide_day: WorldwideDay,
    nod: &EncryptedNodV1,
    (now, sub_series_id): (u64, u16),
    claims: &OfferClaims,
) -> Result<U256> {
    outbe_nod::offers::mark_offer(storage, (worldwide_day, nod.terms.owner), &claims.su_hashes)?;
    let nod_id = crate::runtime::issue_offer_nod(storage, nod, now, sub_series_id)?;
    record_worldwide_day_nod(storage, worldwide_day, nod)?;
    record_agent_reward_activity(storage, now, &claims.wallets, &claims.sras)?;
    Ok(nod_id)
}

/// Whole hours from the start of the offering window to the hour of `now`.
pub(crate) fn offer_hour(now: u64, offerings_start: u64) -> Result<u16> {
    let hour_start = now - now % SECONDS_PER_HOUR;
    let hours = hour_start.saturating_sub(offerings_start) / SECONDS_PER_HOUR;
    Ok(u16::try_from(hours).map_err(|_| crate::errors::NodFactoryError::InvalidEncryptedNod)?)
}

fn record_worldwide_day_nod(
    storage: &StorageHandle<'_>,
    wwd: WorldwideDay,
    nod: &EncryptedNodV1,
) -> Result<()> {
    let totals = outbe_worldwideday::worldwide_day_totals(storage.clone(), wwd)?;
    use outbe_nod::offers::{
        decode_worldwide_day_sum, encode_worldwide_day_sum, worldwide_day_load_scope,
        worldwide_day_nominal_scope,
    };
    let nominal = nod_api::add_to_day_sum(
        storage,
        worldwide_day_nominal_scope(wwd),
        decode_worldwide_day_sum(&totals.encrypted_nominal)?,
        (nod, true),
    )?;
    let load = nod_api::add_to_day_sum(
        storage,
        worldwide_day_load_scope(wwd),
        decode_worldwide_day_sum(&totals.encrypted_load)?,
        (nod, false),
    )?;
    outbe_worldwideday::record_worldwide_day_nod(
        storage.clone(),
        wwd,
        (
            &encode_worldwide_day_sum(&nominal)?,
            &encode_worldwide_day_sum(&load)?,
        ),
    )
}

/// Records one offer in the UTC reward-day bucket that the Cycle reads on the next day.
fn record_agent_reward_activity(
    storage: &StorageHandle<'_>,
    now: u64,
    wallets: &[Address],
    sras: &[Address],
) -> Result<()> {
    if wallets.is_empty() {
        return Ok(());
    }
    let reward_utc_day = WorldwideDay::new(timestamp_to_date_key(now));
    let mut agent_reward = AgentRewardContract::new(storage.clone());
    for address in wallets {
        agent_reward.increment_waa_offer(reward_utc_day, *address)?;
    }
    for address in sras {
        agent_reward.increment_sra_offer(reward_utc_day, *address)?;
    }
    Ok(())
}

fn verified_proof(storage: &StorageHandle<'_>, input: &IssueNodInput) -> Result<VerifiedProof> {
    let zk_check = outbe_l2registry::api::check_zk_merkle_root_signature(
        storage.clone(),
        u64::from(input.l2_chain_id),
        &input.zk_merkle_root,
        &input.signature,
    )?;
    let host_chain_id = storage.chain_id()?;
    match zk_check {
        outbe_l2registry::api::ZkOfferCheck::Verified { .. } => {
            if input.zk_proof.is_empty() {
                return Err(OfferError::ZkProofRequired.into());
            }
            let verification_key =
                resolve_verification_key(host_chain_id, input.l2_chain_id, &input.circuit_version)?;
            let public = decode_zk_public_inputs(&input.zk_proof, verification_key)?;
            if public.merkle_root.as_slice() != input.zk_merkle_root.as_ref() {
                return Err(OfferError::ZkPublicInputMismatch {
                    field: "merkle_root",
                }
                .into());
            }
            Ok(VerifiedProof {
                host_chain_id,
                public,
                verification_key,
            })
        }
        outbe_l2registry::api::ZkOfferCheck::NotRegistered => Err(
            outbe_l2registry::errors::L2RegistryError::NetworkNotRegistered {
                chain_id: u64::from(input.l2_chain_id),
            }
            .into(),
        ),
    }
}

pub(crate) fn pricing(
    storage: &StorageHandle<'_>,
    input: &IssueNodInput,
) -> Result<(
    outbe_worldwideday::WwdProjection,
    outbe_oracle::api::TributePricingInputs,
)> {
    let worldwide_day = input.worldwide_day;
    if !worldwide_day.is_valid() {
        return Err(OfferError::InvalidWorldwideDay { worldwide_day }.into());
    }
    if !outbe_worldwideday::is_offering_day(storage.clone(), worldwide_day)? {
        return Err(OfferError::WorldwideDayNotOffering { worldwide_day }.into());
    }
    let wwd = outbe_worldwideday::worldwide_day(storage.clone(), worldwide_day)?
        .ok_or(OfferError::WorldwideDayNotOffering { worldwide_day })?;
    outbe_oracle::api::check_reference_currency(input.reference_currency)?;
    let pricing = outbe_oracle::api::tribute_pricing_inputs(
        storage.clone(),
        input.issuance_currency,
        input.reference_currency,
    )?
    .ok_or(OfferError::IssuanceCurrencyNotRegistered {
        issuance_currency: input.issuance_currency,
    })?;
    if pricing.issuance_vwap_minor.is_zero() || pricing.reference_vwap_minor.is_zero() {
        return Err(OfferError::CurrencyPriceUnavailable { worldwide_day }.into());
    }
    Ok((wwd, pricing))
}

fn resolve_verification_key(
    host_chain_id: u64,
    l2_chain_id: u32,
    version: &str,
) -> Result<&'static [u8]> {
    let binding = outbe_l2registry::api::l2_circuits(host_chain_id, u64::from(l2_chain_id))
        .iter()
        .find(|entry| entry.version == version)
        .ok_or_else(|| OfferError::UnknownCircuitVersion {
            chain_id: l2_chain_id,
            version: version.to_owned(),
        })?;
    if binding.proof_system != "bb-keccak-v1" {
        return Err(OfferError::UnsupportedZkCircuit.into());
    }
    Ok(binding.vk_bytes)
}

fn decode_zk_public_inputs(proof: &[u8], verification_key: &[u8]) -> Result<OfferPublicInputs> {
    // bb-keccak-v1 VK: log circuit size, public count (including eight
    // pairing-accumulator words), offset, then 28 two-word commitments.
    if verification_key.len() != 59 * 32 {
        return Err(OfferError::UnsupportedZkCircuit.into());
    }
    let log_n = read_u64_be_padded(&verification_key[..32])
        .filter(|size| (1..=28).contains(size))
        .ok_or(OfferError::UnsupportedZkCircuit)?;
    if read_u64_be_padded(&verification_key[32..64]) != Some(4 + 8) {
        return Err(OfferError::UnsupportedZkCircuit.into());
    }
    // UltraKeccakZK: DefaultIO + Oink + Sumcheck + Shplemini.
    let combined_len = 4 + (4 + 82 + 12 * log_n as usize) * 32;
    let words = decode_public_words::<4>(proof, combined_len)
        .map_err(|err| OfferError::MalformedZkProof(err.to_string()))?;
    Ok(OfferPublicInputs::from_raw_parts(words))
}

fn validate_zk_result(
    zk_proof: &[u8],
    verification_key: &[u8],
    public: OfferPublicInputs,
    expected: Option<&OfferZkExpectedHashes>,
) -> Result<()> {
    let expected = expected.ok_or(OfferError::ZkPublicInputMismatch {
        field: "tee_expected_hashes",
    })?;
    if public.nft_hash != expected.nft_hash {
        return Err(OfferError::ZkPublicInputMismatch { field: "nft_hash" }.into());
    }
    if public.binding_hash != expected.binding_hash {
        return Err(OfferError::ZkPublicInputMismatch {
            field: "binding_hash",
        }
        .into());
    }
    let verified = Barretenberg::default()
        .verify_combined(verification_key, zk_proof)
        .map_err(|err| OfferError::ZkVerifierUnavailable(err.to_string()))?;
    if !verified {
        return Err(OfferError::InvalidZkProof.into());
    }
    Ok(())
}

fn parse_su_hashes(su_hashes: &[String]) -> Result<Vec<B256>> {
    su_hashes
        .iter()
        .map(|hash| {
            let hex_str = hash.strip_prefix("0x").unwrap_or(hash);
            let bytes = hex::decode(hex_str)
                .map_err(|_| OfferError::InvalidSuHashHex { hash: hash.clone() })?;
            if bytes.len() != 32 {
                return Err(OfferError::InvalidSuHashLength {
                    length: bytes.len(),
                }
                .into());
            }
            Ok(B256::from_slice(&bytes))
        })
        .collect()
}

fn parse_addresses(
    addresses: &[String],
    invalid: impl Fn(usize, String) -> OfferError,
) -> Result<Vec<Address>> {
    addresses
        .iter()
        .enumerate()
        .map(|(index, address)| {
            address
                .parse::<Address>()
                .map_err(|_| invalid(index, address.clone()).into())
        })
        .collect()
}

pub(crate) fn validate_agent_reward_addresses(
    wallet_addresses: &[String],
    sra_addresses: &[String],
) -> Result<(Vec<Address>, Vec<Address>)> {
    match (wallet_addresses.is_empty(), sra_addresses.is_empty()) {
        (true, true) => Ok((Vec::new(), Vec::new())),
        (true, false) => Err(OfferError::WalletAddressesRequiredWhenSraProvided.into()),
        (false, true) => Err(OfferError::SraAddressesRequiredWhenWalletProvided.into()),
        (false, false) => Ok((
            parse_addresses(wallet_addresses, |index, address| {
                OfferError::InvalidWalletAddressAtIndex { index, address }
            })?,
            parse_addresses(sra_addresses, |index, address| {
                OfferError::InvalidSraAddressAtIndex { index, address }
            })?,
        )),
    }
}
