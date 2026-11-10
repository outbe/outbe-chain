//! Stateless NOD encryption under the `NOD` confidential domain.
use crate::{
    confidential::{SlotCipherDomain, NOD},
    crypto::hkdf_sha256,
    errors::{Result, TeeError, TributeDayRejectReason},
    tribute_encryption::decrypt_tribute,
};
use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    nod_encryption::{
        EncryptedNodGroupLoadV1, EncryptedNodV1, NodQuoteContextV1, NodTermsV1,
        NOD_AMOUNT_NONCE_INFO, NOD_QUOTE_NONCE_INFO,
    },
    tribute_encryption::EncryptedTributeV2,
};
use outbe_tee::nod_mine::{
    nod_group_load_inputs_hash, IssueNodFromTributeOutcomeV1, IssueNodFromTributeRequestV1,
    NodGroupLoadOpRequestV1, NodGroupLoadOperationV1,
};
use ring::hmac;
use zeroize::Zeroizing;

const AMOUNT: SlotCipherDomain = SlotCipherDomain {
    nonce_info: NOD_AMOUNT_NONCE_INFO,
};
const GROUP_LOAD: SlotCipherDomain = SlotCipherDomain {
    nonce_info: b"outbe/nod/group-load-nonce/v1",
};
const QUOTE_AMOUNT: SlotCipherDomain = SlotCipherDomain {
    nonce_info: NOD_QUOTE_NONCE_INFO,
};

/// Derive the resident NOD state key from the DKG group signature.
pub fn derive_nod_state_key(
    group_sig: &[u8],
    chain_id: B256,
    epoch: u64,
) -> Result<Zeroizing<[u8; 32]>> {
    NOD.derive_state_key(group_sig, chain_id, epoch)
}

/// Derive the per-owner NOD view key from `nod_state_key`.
pub fn derive_view_key(state_key: &[u8; 32], owner: Address) -> Result<Zeroizing<[u8; 32]>> {
    NOD.account_keys.derive_view_key(state_key, owner)
}

/// The caller authenticates the certified action and source membership first.
pub fn encrypt_nod_for_tribute(
    tribute_state_key: &[u8; 32],
    nod_state_key: &[u8; 32],
    tribute: &EncryptedTributeV2,
    terms: NodTermsV1,
    amount: U256,
) -> Result<EncryptedNodV1> {
    let source = &tribute.context;
    let source_terms = (
        source.chain_id,
        source.owner,
        source.issuance_currency,
        source.reference_currency,
    );
    let issued_terms = (
        terms.chain_id,
        terms.owner,
        terms.issuance_currency,
        terms.reference_currency,
    );
    let canonical_id = terms
        .series_id()
        .map(|series_id| outbe_common::nod_rules::derive_nod_id(series_id, terms.owner));
    if issued_terms != source_terms || canonical_id != Ok(terms.nod_id) {
        return Err(TeeError::TributeOfferReject(
            "NOD source terms mismatch".into(),
        ));
    }
    let _ = decrypt_tribute(tribute_state_key, tribute)?;
    encrypt_nod(nod_state_key, terms, amount)
}

/// Encrypt already authenticated immutable terms; production issuance supplies a verified source.
pub fn encrypt_nod(
    nod_state_key: &[u8; 32],
    terms: NodTermsV1,
    amount: U256,
) -> Result<EncryptedNodV1> {
    let mut nod = EncryptedNodV1 {
        encryption_binding: binding(nod_state_key, &terms, amount),
        terms,
        encrypted_gratis_amount: Vec::new(),
    };
    let key = amount_key(nod_state_key, &nod)?;
    let plaintext = Zeroizing::new(amount.to_be_bytes::<32>());
    nod.encrypted_gratis_amount = AMOUNT
        .slot(&key, nod.crypto_slot(), 0)
        .write_blob(0, plaintext.as_ref())?;
    Ok(nod)
}

pub fn decrypt_nod(nod_state_key: &[u8; 32], nod: &EncryptedNodV1) -> Result<U256> {
    if !nod.has_valid_encoding() {
        return Err(TeeError::DecryptFailed);
    }
    let key = amount_key(nod_state_key, nod)?;
    let (_, plaintext) = AMOUNT
        .slot(&key, nod.crypto_slot(), 0)
        .read_blob(&nod.encrypted_gratis_amount)?;
    let plaintext = Zeroizing::new(plaintext);
    let amount: &[u8; 32] = plaintext
        .as_slice()
        .try_into()
        .map_err(|_| TeeError::DecryptFailed)?;
    let amount = U256::from_be_bytes(*amount);
    if binding(nod_state_key, &nod.terms, amount) != nod.encryption_binding {
        return Err(TeeError::DecryptFailed);
    }
    Ok(amount)
}

fn binding(nod_state_key: &[u8; 32], terms: &NodTermsV1, amount: U256) -> B256 {
    let mut input = b"outbe/nod/issuance-binding/v2".to_vec();
    input.extend_from_slice(terms.digest().as_slice());
    input.extend_from_slice(&amount.to_be_bytes::<32>());
    let input = Zeroizing::new(input);
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, nod_state_key), &input);
    B256::from_slice(tag.as_ref())
}

fn amount_key(nod_state_key: &[u8; 32], nod: &EncryptedNodV1) -> Result<Zeroizing<[u8; 32]>> {
    let view_key = derive_view_key(nod_state_key, nod.terms.owner)?;
    Ok(Zeroizing::new(hkdf_sha256(
        nod.context_digest().as_slice(),
        &*view_key,
        &nod.amount_key_info(),
    )?))
}

pub fn issue_nod_from_tribute(
    network_secret: &[u8; 32],
    tribute_state_key: &[u8; 32],
    nod_state_key: &[u8; 32],
    request: &IssueNodFromTributeRequestV1,
) -> Result<IssueNodFromTributeOutcomeV1> {
    let source = &request.tribute.context;
    let terms = &request.terms;
    let source_terms = (
        source.chain_id,
        source.owner,
        source.issuance_currency,
        source.reference_currency,
    );
    let issued_terms = (
        terms.chain_id,
        terms.owner,
        terms.issuance_currency,
        terms.reference_currency,
    );
    let canonical_id = terms
        .series_id()
        .map(|series_id| outbe_common::nod_rules::derive_nod_id(series_id, terms.owner));
    if issued_terms != source_terms
        || canonical_id != Ok(terms.nod_id)
        || request.day_fractions.chain_id != terms.chain_id
        || request.day_fractions.worldwide_day != source.worldwide_day
    {
        return Err(TeeError::TributeOfferReject(
            "NOD source terms mismatch".into(),
        ));
    }
    let amounts = decrypt_tribute(tribute_state_key, &request.tribute)?;
    let fractions = crate::tribute_day::read_day_fractions(network_secret, &request.day_fractions)?;
    let fraction = U256::from(fractions.get(&terms.league_id.max(1)).copied().unwrap_or(0));
    let gratis_load_minor = match outbe_lysis::program_v1::calculate_gratis_load(
        amounts.nominal_amount_minor,
        fraction,
        0,
    ) {
        Ok(load) => load,
        Err(err) => {
            return Ok(IssueNodFromTributeOutcomeV1::LysisArithmetic(
                err.to_string(),
            ))
        }
    };
    let cost =
        outbe_common::nod_rules::settlement_cost_minor(terms.entry_price_minor, gratis_load_minor)
            .map_err(|_| TeeError::TributeOfferReject("settlement cost overflow".into()))?;
    if cost.is_zero() {
        return Ok(IssueNodFromTributeOutcomeV1::ZeroSettlementCost);
    }
    let nod = encrypt_nod(nod_state_key, terms.clone(), gratis_load_minor)?;
    Ok(IssueNodFromTributeOutcomeV1::Issued(nod))
}

pub fn seal_group_load(
    nod_state_key: &[u8; 32],
    (chain_id, scope): (u64, B256),
    (previous_version, inputs_hash): (u64, B256),
    amount: U256,
) -> Result<EncryptedNodGroupLoadV1> {
    let mut operation_context = b"outbe/nod/group-load-operation/v1".to_vec();
    operation_context.extend_from_slice(scope.as_slice());
    operation_context.extend_from_slice(inputs_hash.as_slice());
    let tag = hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA256, nod_state_key),
        &operation_context,
    );
    let mut record = EncryptedNodGroupLoadV1 {
        chain_id,
        scope,
        operation_hash: B256::from_slice(tag.as_ref()),
        encrypted_load: Vec::new(),
    };
    record.encrypted_load = GROUP_LOAD
        .slot(nod_state_key, record.crypto_slot(), 0)
        .write_u256(previous_version, amount)?;
    Ok(record)
}

pub fn read_nod_group_load(
    nod_state_key: &[u8; 32],
    record: &EncryptedNodGroupLoadV1,
) -> Result<U256> {
    GROUP_LOAD
        .slot(nod_state_key, record.crypto_slot(), 0)
        .read_u256(record.version(), &record.encrypted_load)
}

pub fn apply_nod_group_load_operation(
    nod_state_key: &[u8; 32],
    request: &NodGroupLoadOpRequestV1,
) -> Result<EncryptedNodGroupLoadV1> {
    let (previous_version, amount) = match &request.previous {
        None => (0, U256::ZERO),
        Some(previous) => {
            if previous.chain_id != request.chain_id || previous.scope != request.scope {
                return Err(TributeDayRejectReason::InvalidNodGroupLoadPredecessor.into());
            }
            (
                previous.version().ok_or(TeeError::DecryptFailed)?,
                read_nod_group_load(nod_state_key, previous)?,
            )
        }
    };
    let (delta, add) = match &request.operation {
        NodGroupLoadOperationV1::AddSeriesLoad { series_load } => {
            if series_load.chain_id != request.chain_id {
                return Err(TributeDayRejectReason::InvalidNodGroupLoadPredecessor.into());
            }
            (read_nod_group_load(nod_state_key, series_load)?, true)
        }
        NodGroupLoadOperationV1::AddNod { nod } => {
            if nod.terms.chain_id != request.chain_id {
                return Err(TributeDayRejectReason::InvalidNodGroupLoadPredecessor.into());
            }
            (decrypt_nod(nod_state_key, nod)?, true)
        }
        NodGroupLoadOperationV1::SubNod { nod } => {
            if nod.terms.chain_id != request.chain_id {
                return Err(TributeDayRejectReason::InvalidNodGroupLoadPredecessor.into());
            }
            (decrypt_nod(nod_state_key, nod)?, false)
        }
        NodGroupLoadOperationV1::AdjustTransient { load_minor, add } => (*load_minor, *add),
    };
    let next = if add {
        amount.checked_add(delta)
    } else {
        amount.checked_sub(delta)
    }
    .ok_or_else(|| TeeError::from(TributeDayRejectReason::NodGroupLoadOverflowOrUnderflow))?;
    let inputs_hash = nod_group_load_inputs_hash(request).map_err(|_| TeeError::EncryptFailed)?;
    seal_group_load(
        nod_state_key,
        (request.chain_id, request.scope),
        (previous_version, inputs_hash),
        next,
    )
}

pub fn encrypt_nod_quote(
    nod_state_key: &[u8; 32],
    nod: &EncryptedNodV1,
    context: &NodQuoteContextV1,
    payment_minor: U256,
) -> Result<Vec<u8>> {
    if nod.terms.chain_id != context.chain_id
        || nod.terms.nod_id != context.nod_id
        || nod.terms.owner != context.owner
    {
        return Err(TeeError::TributeOfferReject(
            "NOD quote context mismatch".into(),
        ));
    }
    let _ = decrypt_nod(nod_state_key, nod)?;
    let view_key = derive_view_key(nod_state_key, context.owner)?;
    let key = Zeroizing::new(hkdf_sha256(
        context.context_digest().as_slice(),
        &*view_key,
        &context.quote_key_info(),
    )?);
    let plaintext = Zeroizing::new(payment_minor.to_be_bytes::<32>());
    QUOTE_AMOUNT
        .slot(&key, context.crypto_slot(), 0)
        .write_blob(0, plaintext.as_ref())
}
