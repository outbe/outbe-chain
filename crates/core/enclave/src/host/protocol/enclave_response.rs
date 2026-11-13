use alloy_primitives::{Address, B256, U256};
use outbe_tee::{protocol::EnclaveResponse as WireResponse, TransportError};

use super::{
    EncryptedTributeOffer, FidelityCohortRequest, FidelityCohortResult, FidelityLeagueEntry,
    FidelityQueryRequest, FidelityQueryResult, FidelitySnapshotRequest, GratisOpRequest,
    GratisOpResult, Ledger, MineGratisFromPromisRequestV1, MineGratisFromPromisResultV1,
    PromisOpRequest, PromisOpResult, TributeOfferResult,
};

/// Domain-tagged message an account owner personal-signs to authorize Fidelity index queries.
#[must_use]
pub fn fidelity_query_auth_message(chain_id: B256, account: Address, expiry: u64) -> Vec<u8> {
    let tag: &[u8] = b"outbe/fidelity/query-auth/v1";
    let mut m = Vec::with_capacity(tag.len() + 32 + 20 + 8);
    m.extend_from_slice(tag);
    m.extend_from_slice(chain_id.as_slice());
    m.extend_from_slice(account.as_slice());
    m.extend_from_slice(&expiry.to_be_bytes());
    m
}

/// Deterministic hash over the canonical batch inputs: every field of every offer.
#[must_use]
pub fn inputs_canonical_hash(offers: &[EncryptedTributeOffer]) -> B256 {
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(&(offers.len() as u32).to_be_bytes());
    for offer in offers {
        buf.extend_from_slice(offer.owner.as_slice());
        buf.extend_from_slice(&(offer.cipher_text.len() as u32).to_be_bytes());
        buf.extend_from_slice(&offer.cipher_text);
        buf.extend_from_slice(&(offer.nonce.len() as u32).to_be_bytes());
        buf.extend_from_slice(&offer.nonce);
        buf.extend_from_slice(&offer.ephemeral_pubkey.to_be_bytes::<32>());
        buf.extend_from_slice(&offer.worldwide_day.value().to_be_bytes());
        buf.extend_from_slice(&offer.tribute_currency.to_be_bytes());
        buf.extend_from_slice(&offer.reference_currency.to_be_bytes());
        buf.extend_from_slice(&offer.issuance_wwd_vwap_minor.to_be_bytes::<32>());
        buf.extend_from_slice(&offer.reference_wwd_vwap_minor.to_be_bytes::<32>());
        buf.extend_from_slice(&offer.reference_scurve_minor.to_be_bytes::<32>());
        match &offer.zk_context {
            Some(context) => {
                buf.push(1);
                buf.extend_from_slice(context.derived_owner.as_slice());
                buf.extend_from_slice(&context.chain_id.to_be_bytes());
                buf.extend_from_slice(&context.l2_chain_id.to_be_bytes());
            }
            None => buf.push(0),
        }
    }
    alloy_primitives::keccak256(buf)
}

/// Domain-tagged message a caller personal-signs to prove control of `account` before `DeriveAccountKeys`.
#[must_use]
pub fn derive_account_keys_message(
    ledger: Ledger,
    account: Address,
    ephemeral_pubkey: B256,
) -> Vec<u8> {
    let tag: &[u8] = match ledger {
        Ledger::Gratis => b"outbe/gratis/derive-keys/v1",
        Ledger::Promis => b"outbe/promis/derive-keys/v1",
        Ledger::Fidelity => b"outbe/fidelity/derive-keys/v1",
        Ledger::Tribute => b"outbe/tribute/derive-keys/v1",
        Ledger::Nod => b"outbe/nod/derive-keys/v1",
    };
    let mut m = Vec::with_capacity(tag.len() + 20 + 32);
    m.extend_from_slice(tag);
    m.extend_from_slice(account.as_slice());
    m.extend_from_slice(ephemeral_pubkey.as_slice());
    m
}

/// Backward-compatible alias for the Gratis key-derivation message.
#[must_use]
pub fn derive_gratis_keys_message(account: Address, ephemeral_pubkey: B256) -> Vec<u8> {
    derive_account_keys_message(Ledger::Gratis, account, ephemeral_pubkey)
}

/// Domain-separated preimage signed by the enclave for `TributeOfferBatch`.
pub fn tribute_offer_attestation_preimage(
    inputs_canonical_hash: B256,
    results: &[TributeOfferResult],
) -> Result<Vec<u8>, serde_json::Error> {
    let results_json = serde_json::to_vec(results)?;
    let mut buf = Vec::with_capacity(30 + 32 + 4 + results_json.len());
    buf.extend_from_slice(b"outbe/tee/offer-attestation/v1");
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(results_json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&results_json);
    Ok(buf)
}

/// Domain responses carried inside [`outbe_tee::protocol::EnclaveResponse::Payload`].
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EnclaveResponse {
    TributeOfferBatch {
        results: Vec<TributeOfferResult>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    GratisOpApplied {
        result: Box<GratisOpResult>,
    },
    PromisOpApplied {
        result: Box<PromisOpResult>,
    },
    FidelityCohortApplied {
        result: Box<FidelityCohortResult>,
    },
    FidelityLeaguesSnapshotted {
        leagues: Vec<FidelityLeagueEntry>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    FidelityIndexQueried {
        result: Box<FidelityQueryResult>,
    },
    AccountKeysSealed {
        account: Address,
        sealed: Vec<u8>,
        nonce: [u8; 12],
        enclave_ephemeral_pubkey: [u8; 32],
    },
    NodEncryptedForTributeV1 {
        nod: outbe_core_common::nod_encryption::EncryptedNodV1,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    EncryptedNodMintRejectedV1 {
        reason: String,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    EncryptedNodMinedV1 {
        result: Box<crate::host::nod_mine::MineEncryptedNodResultV1>,
    },
    NodAmountReadV1 {
        amount: U256,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    EncryptedTributeOfferBatchV2 {
        results: Vec<crate::host::tribute_v2::EncryptedTributeOfferResultV2>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    TributeAmountsReadV2 {
        amounts: Vec<outbe_core_common::tribute_encryption::TributeAmountsV2>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    TributeDayOpAppliedV2 {
        record: outbe_core_common::tribute_day_encryption::EncryptedTributeDayAmountV2,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    TributeDayAmountReadV2 {
        amount: U256,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    FidelityQueryRejected {
        inputs_canonical_hash: B256,
        reason: String,
        attestation_tag: Vec<u8>,
    },
    LysisFractionsExecutedV1 {
        result: Box<crate::host::tribute_day::LysisFractionsResultV1>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    TributeDayFractionsReadV1 {
        fractions: std::collections::BTreeMap<u16, u64>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    NodIssuedFromTributeV1 {
        outcome: crate::host::nod_mine::IssueNodFromTributeOutcomeV1,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    NodGroupLoadOpAppliedV1 {
        record: outbe_core_common::nod_encryption::EncryptedNodGroupLoadV1,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    NodGroupLoadReadV1 {
        amount: U256,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    NodQuoteEncryptedV1 {
        encrypted_payment: Vec<u8>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    GratisMinedFromPromisV1 {
        result: Box<MineGratisFromPromisResultV1>,
    },
}

impl EnclaveResponse {
    /// True when this response variant is recorded on the block's consensus witness tape.
    #[must_use]
    pub const fn is_recordable(&self) -> bool {
        !matches!(
            self,
            Self::TributeOfferBatch { .. }
                | Self::FidelityCohortApplied { .. }
                | Self::AccountKeysSealed { .. }
        )
    }

    /// True when this response carries an `inputs_canonical_hash` and Ed25519 `attestation_tag`.
    #[must_use]
    pub const fn is_signed_domain_response(&self) -> bool {
        !matches!(self, Self::AccountKeysSealed { .. })
    }

    fn signed_parts(&self) -> Option<(B256, &[u8])> {
        match self {
            Self::GratisOpApplied { result } => {
                Some((result.inputs_canonical_hash, &result.attestation_tag))
            }
            Self::PromisOpApplied { result } => {
                Some((result.inputs_canonical_hash, &result.attestation_tag))
            }
            Self::FidelityCohortApplied { result } => {
                Some((result.inputs_canonical_hash, &result.attestation_tag))
            }
            Self::FidelityIndexQueried { result } => {
                Some((result.inputs_canonical_hash, &result.attestation_tag))
            }
            Self::EncryptedNodMinedV1 { result } => {
                Some((result.inputs_canonical_hash, &result.attestation_tag))
            }
            Self::GratisMinedFromPromisV1 { result } => {
                Some((result.inputs_canonical_hash, &result.attestation_tag))
            }
            Self::TributeOfferBatch {
                inputs_canonical_hash,
                attestation_tag,
                ..
            }
            | Self::FidelityLeaguesSnapshotted {
                inputs_canonical_hash,
                attestation_tag,
                ..
            }
            | Self::NodEncryptedForTributeV1 {
                inputs_canonical_hash,
                attestation_tag,
                ..
            }
            | Self::EncryptedNodMintRejectedV1 {
                inputs_canonical_hash,
                attestation_tag,
                ..
            }
            | Self::NodAmountReadV1 {
                inputs_canonical_hash,
                attestation_tag,
                ..
            }
            | Self::EncryptedTributeOfferBatchV2 {
                inputs_canonical_hash,
                attestation_tag,
                ..
            }
            | Self::TributeAmountsReadV2 {
                inputs_canonical_hash,
                attestation_tag,
                ..
            }
            | Self::TributeDayOpAppliedV2 {
                inputs_canonical_hash,
                attestation_tag,
                ..
            }
            | Self::TributeDayAmountReadV2 {
                inputs_canonical_hash,
                attestation_tag,
                ..
            }
            | Self::FidelityQueryRejected {
                inputs_canonical_hash,
                attestation_tag,
                ..
            }
            | Self::LysisFractionsExecutedV1 {
                inputs_canonical_hash,
                attestation_tag,
                ..
            }
            | Self::TributeDayFractionsReadV1 {
                inputs_canonical_hash,
                attestation_tag,
                ..
            }
            | Self::NodIssuedFromTributeV1 {
                inputs_canonical_hash,
                attestation_tag,
                ..
            }
            | Self::NodGroupLoadOpAppliedV1 {
                inputs_canonical_hash,
                attestation_tag,
                ..
            }
            | Self::NodGroupLoadReadV1 {
                inputs_canonical_hash,
                attestation_tag,
                ..
            }
            | Self::NodQuoteEncryptedV1 {
                inputs_canonical_hash,
                attestation_tag,
                ..
            } => Some((*inputs_canonical_hash, attestation_tag)),
            Self::AccountKeysSealed { .. } => None,
        }
    }

    fn signed_tag_mut(&mut self) -> Option<&mut Vec<u8>> {
        match self {
            Self::GratisOpApplied { result } => Some(&mut result.attestation_tag),
            Self::PromisOpApplied { result } => Some(&mut result.attestation_tag),
            Self::FidelityCohortApplied { result } => Some(&mut result.attestation_tag),
            Self::FidelityIndexQueried { result } => Some(&mut result.attestation_tag),
            Self::EncryptedNodMinedV1 { result } => Some(&mut result.attestation_tag),
            Self::GratisMinedFromPromisV1 { result } => Some(&mut result.attestation_tag),
            Self::TributeOfferBatch {
                attestation_tag, ..
            }
            | Self::FidelityLeaguesSnapshotted {
                attestation_tag, ..
            }
            | Self::NodEncryptedForTributeV1 {
                attestation_tag, ..
            }
            | Self::EncryptedNodMintRejectedV1 {
                attestation_tag, ..
            }
            | Self::NodAmountReadV1 {
                attestation_tag, ..
            }
            | Self::EncryptedTributeOfferBatchV2 {
                attestation_tag, ..
            }
            | Self::TributeAmountsReadV2 {
                attestation_tag, ..
            }
            | Self::TributeDayOpAppliedV2 {
                attestation_tag, ..
            }
            | Self::TributeDayAmountReadV2 {
                attestation_tag, ..
            }
            | Self::FidelityQueryRejected {
                attestation_tag, ..
            }
            | Self::LysisFractionsExecutedV1 {
                attestation_tag, ..
            }
            | Self::TributeDayFractionsReadV1 {
                attestation_tag, ..
            }
            | Self::NodIssuedFromTributeV1 {
                attestation_tag, ..
            }
            | Self::NodGroupLoadOpAppliedV1 {
                attestation_tag, ..
            }
            | Self::NodGroupLoadReadV1 {
                attestation_tag, ..
            }
            | Self::NodQuoteEncryptedV1 {
                attestation_tag, ..
            } => Some(attestation_tag),
            Self::AccountKeysSealed { .. } => None,
        }
    }

    /// Return the canonical input commitment carried by this response (`B256::ZERO` if unsigned).
    #[must_use]
    pub fn inputs_canonical_hash(&self) -> B256 {
        self.signed_parts()
            .map_or(B256::ZERO, |(inputs_hash, _)| inputs_hash)
    }

    /// Borrow the response's Ed25519 attestation signature (`&[]` if unsigned or stripped).
    #[must_use]
    pub fn attestation_tag(&self) -> &[u8] {
        self.signed_parts().map_or(&[], |(_, tag)| tag)
    }

    fn take_attestation_tag(&mut self) -> Vec<u8> {
        self.signed_tag_mut().map_or_else(Vec::new, core::mem::take)
    }

    fn set_attestation_tag(&mut self, tag: Vec<u8>) {
        if let Some(slot) = self.signed_tag_mut() {
            *slot = tag;
        }
    }

    /// Convert this domain response into a canonical tag-free [`WireResponse`].
    ///
    /// Every inner `attestation_tag` (including nested result structs) is cleared
    /// before `postcard` encoding so `Payload.payload` is byte-identical across
    /// proposers and verifiers holding distinct Ed25519 attestation keys.
    pub fn into_wire_response(mut self) -> Result<WireResponse, TransportError> {
        let recordable = self.is_recordable();
        let inputs_canonical_hash = self.inputs_canonical_hash();
        let attestation_tag = self.take_attestation_tag();
        let payload = postcard::to_allocvec(&self).map_err(TransportError::codec)?;
        Ok(WireResponse::Payload {
            recordable,
            payload,
            inputs_canonical_hash,
            attestation_tag,
        })
    }

    /// Strictly decode and canonically verify a [`WireResponse`] back into [`Self`].
    pub fn from_wire_response(wire: &WireResponse) -> Result<Self, TransportError> {
        match wire {
            WireResponse::Error { message } => Err(TransportError::EnclaveError(message.clone())),
            WireResponse::NotReady { message } => Err(TransportError::Unavailable(message.clone())),
            WireResponse::Payload {
                recordable,
                payload,
                inputs_canonical_hash,
                attestation_tag,
            } => Self::decode_wire_payload(
                *recordable,
                payload,
                *inputs_canonical_hash,
                attestation_tag,
            ),
            _ => Err(TransportError::UnexpectedResponse),
        }
    }

    fn decode_wire_payload(
        recordable: bool,
        payload: &[u8],
        inputs_canonical_hash: B256,
        attestation_tag: &[u8],
    ) -> Result<Self, TransportError> {
        let mut decoded = Self::decode_canonical_tag_free(payload)?;
        Self::verify_wire_metadata(&decoded, recordable, inputs_canonical_hash, attestation_tag)?;
        decoded.set_attestation_tag(attestation_tag.to_vec());
        Ok(decoded)
    }

    fn decode_canonical_tag_free(payload: &[u8]) -> Result<Self, TransportError> {
        let (decoded, rest) =
            postcard::take_from_bytes::<Self>(payload).map_err(TransportError::codec)?;
        if !rest.is_empty() {
            return Err(TransportError::Codec(
                "trailing bytes in enclave response payload".into(),
            ));
        }
        if !decoded.attestation_tag().is_empty() {
            return Err(TransportError::Codec(
                "non-empty inner attestation_tag in enclave payload".into(),
            ));
        }
        let reencoded = postcard::to_allocvec(&decoded).map_err(TransportError::codec)?;
        if reencoded != payload {
            return Err(TransportError::Codec(
                "non-canonical enclave response payload".into(),
            ));
        }
        Ok(decoded)
    }

    fn verify_wire_metadata(
        decoded: &Self,
        recordable: bool,
        inputs_canonical_hash: B256,
        attestation_tag: &[u8],
    ) -> Result<(), TransportError> {
        if decoded.is_recordable() != recordable
            || decoded.inputs_canonical_hash() != inputs_canonical_hash
        {
            return Err(TransportError::Codec(
                "enclave response payload metadata mismatch".into(),
            ));
        }
        if !decoded.is_signed_domain_response()
            && (!inputs_canonical_hash.is_zero() || !attestation_tag.is_empty())
        {
            return Err(TransportError::Codec(
                "unsigned enclave payload carries commitment or tag".into(),
            ));
        }
        Ok(())
    }
}

impl core::fmt::Debug for EnclaveResponse {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::LysisFractionsExecutedV1 {
                inputs_canonical_hash,
                ..
            } => f
                .debug_struct("LysisFractionsExecutedV1")
                .field("result", &"<redacted>")
                .field("inputs_canonical_hash", inputs_canonical_hash)
                .finish(),
            Self::TributeDayFractionsReadV1 {
                inputs_canonical_hash,
                ..
            } => f
                .debug_struct("TributeDayFractionsReadV1")
                .field("fractions", &"<redacted>")
                .field("inputs_canonical_hash", inputs_canonical_hash)
                .finish(),
            Self::NodIssuedFromTributeV1 {
                inputs_canonical_hash,
                ..
            } => f
                .debug_struct("NodIssuedFromTributeV1")
                .field("outcome", &"<redacted>")
                .field("inputs_canonical_hash", inputs_canonical_hash)
                .finish(),
            Self::NodGroupLoadOpAppliedV1 {
                record,
                inputs_canonical_hash,
                ..
            } => f
                .debug_struct("NodGroupLoadOpAppliedV1")
                .field("scope", &record.scope)
                .field("encrypted_load", &"<redacted>")
                .field("inputs_canonical_hash", inputs_canonical_hash)
                .finish(),
            Self::NodGroupLoadReadV1 {
                inputs_canonical_hash,
                ..
            } => f
                .debug_struct("NodGroupLoadReadV1")
                .field("amount", &"<redacted>")
                .field("inputs_canonical_hash", inputs_canonical_hash)
                .finish(),
            Self::NodQuoteEncryptedV1 {
                inputs_canonical_hash,
                ..
            } => f
                .debug_struct("NodQuoteEncryptedV1")
                .field("encrypted_payment", &"<redacted>")
                .field("inputs_canonical_hash", inputs_canonical_hash)
                .finish(),
            _ => f.debug_struct("EnclaveResponse").finish_non_exhaustive(),
        }
    }
}

/// Deterministic hash over the canonical inputs of a single Gratis op.
#[must_use]
pub fn gratis_op_canonical_hash(req: &GratisOpRequest) -> B256 {
    fn push_bytes(buf: &mut Vec<u8>, b: &[u8]) {
        buf.extend_from_slice(&(b.len() as u32).to_be_bytes());
        buf.extend_from_slice(b);
    }
    let mut buf: Vec<u8> = Vec::new();
    buf.push(req.op as u8);
    buf.extend_from_slice(req.chain_id.as_slice());
    buf.extend_from_slice(req.account.as_slice());
    buf.extend_from_slice(&req.amount.to_be_bytes::<32>());
    push_bytes(&mut buf, &req.current_balance);
    push_bytes(&mut buf, &req.current_pledged);
    buf.extend_from_slice(&req.modify_auth.mac);
    buf.extend_from_slice(&req.modify_auth.op_nonce.to_be_bytes());
    match &req.fidelity {
        Some(f) => {
            buf.push(1);
            buf.push(f.op as u8);
            buf.extend_from_slice(&f.timestamp.to_be_bytes());
            buf.extend_from_slice(&f.first_qualified_start.to_be_bytes());
            push_bytes(&mut buf, &f.current_blob);
        }
        None => buf.push(0),
    }
    alloy_primitives::keccak256(buf)
}

/// Domain-separated preimage signed by the enclave for a Gratis op.
pub fn gratis_op_attestation_preimage(
    inputs_canonical_hash: B256,
    result: &GratisOpResult,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut probe = result.clone();
    probe.attestation_tag = Vec::new();
    let result_json = serde_json::to_vec(&probe)?;
    let mut buf = Vec::with_capacity(31 + 32 + 4 + result_json.len());
    buf.extend_from_slice(b"outbe/tee/gratis-attestation/v3");
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(result_json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&result_json);
    Ok(buf)
}

/// Deterministic hash over the canonical inputs of a single Promis op.
#[must_use]
pub fn promis_op_canonical_hash(req: &PromisOpRequest) -> B256 {
    let mut buf: Vec<u8> = Vec::new();
    buf.push(req.op as u8);
    buf.extend_from_slice(req.chain_id.as_slice());
    buf.extend_from_slice(req.account.as_slice());
    buf.extend_from_slice(&req.amount.to_be_bytes::<32>());
    buf.extend_from_slice(&(req.current_balance.len() as u32).to_be_bytes());
    buf.extend_from_slice(&req.current_balance);
    buf.extend_from_slice(&req.modify_auth.mac);
    buf.extend_from_slice(&req.modify_auth.op_nonce.to_be_bytes());
    alloy_primitives::keccak256(buf)
}

/// Domain-separated preimage signed by the enclave for a Promis op.
pub fn promis_op_attestation_preimage(
    inputs_canonical_hash: B256,
    result: &PromisOpResult,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut probe = result.clone();
    probe.attestation_tag = Vec::new();
    let result_json = serde_json::to_vec(&probe)?;
    let mut buf = Vec::with_capacity(31 + 32 + 4 + result_json.len());
    buf.extend_from_slice(b"outbe/tee/promis-attestation/v1");
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(result_json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&result_json);
    Ok(buf)
}

/// Deterministic hash over the canonical inputs of [`MineGratisFromPromisRequestV1`].
#[must_use]
pub fn mine_gratis_from_promis_canonical_hash(req: &MineGratisFromPromisRequestV1) -> B256 {
    fn push_bytes(buf: &mut Vec<u8>, b: &[u8]) {
        buf.extend_from_slice(&(b.len() as u32).to_be_bytes());
        buf.extend_from_slice(b);
    }
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(req.chain_id.as_slice());
    buf.extend_from_slice(req.account.as_slice());
    push_bytes(&mut buf, &req.encrypted_promis_amount);
    push_bytes(&mut buf, &req.current_promis_balance);
    buf.extend_from_slice(&req.promis_modify_auth.mac);
    buf.extend_from_slice(&req.promis_modify_auth.op_nonce.to_be_bytes());
    push_bytes(&mut buf, &req.current_gratis_balance);
    buf.extend_from_slice(&req.gratis_modify_auth.mac);
    buf.extend_from_slice(&req.gratis_modify_auth.op_nonce.to_be_bytes());
    buf.push(req.fidelity.op as u8);
    buf.extend_from_slice(&req.fidelity.timestamp.to_be_bytes());
    buf.extend_from_slice(&req.fidelity.first_qualified_start.to_be_bytes());
    push_bytes(&mut buf, &req.fidelity.current_blob);
    alloy_primitives::keccak256(buf)
}

/// Domain-separated attestation preimage for [`MineGratisFromPromisResultV1`].
pub fn mine_gratis_from_promis_attestation_preimage(
    inputs_canonical_hash: B256,
    result: &MineGratisFromPromisResultV1,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut probe = result.clone();
    probe.attestation_tag = Vec::new();
    let result_json = serde_json::to_vec(&probe)?;
    let tag = b"outbe/tee/mine-gratis-from-promis-attestation/v1";
    let mut buf = Vec::with_capacity(tag.len() + 32 + 4 + result_json.len());
    buf.extend_from_slice(tag);
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(result_json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&result_json);
    Ok(buf)
}

/// Deterministic hash over the canonical inputs of a standalone Fidelity cohort op.
#[must_use]
pub fn fidelity_cohort_canonical_hash(req: &FidelityCohortRequest) -> B256 {
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(req.chain_id.as_slice());
    buf.extend_from_slice(req.account.as_slice());
    buf.extend_from_slice(&req.amount.to_be_bytes::<32>());
    buf.push(req.section.op as u8);
    buf.extend_from_slice(&req.section.timestamp.to_be_bytes());
    buf.extend_from_slice(&req.section.first_qualified_start.to_be_bytes());
    buf.extend_from_slice(&(req.section.current_blob.len() as u32).to_be_bytes());
    buf.extend_from_slice(&req.section.current_blob);
    alloy_primitives::keccak256(buf)
}

/// Domain-separated attestation preimage for a standalone Fidelity cohort op.
pub fn fidelity_cohort_attestation_preimage(
    inputs_canonical_hash: B256,
    result: &FidelityCohortResult,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut probe = result.clone();
    probe.attestation_tag = Vec::new();
    let result_json = serde_json::to_vec(&probe)?;
    let mut buf = Vec::with_capacity(39 + 32 + 4 + result_json.len());
    buf.extend_from_slice(b"outbe/tee/fidelity-cohort-attestation/v1");
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(result_json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&result_json);
    Ok(buf)
}

/// Deterministic hash over the canonical inputs of a Fidelity league snapshot batch.
#[must_use]
pub fn fidelity_snapshot_canonical_hash(req: &FidelitySnapshotRequest) -> B256 {
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(&req.timestamp.to_be_bytes());
    buf.extend_from_slice(&req.first_qualified_start.to_be_bytes());
    buf.extend_from_slice(&(req.entries.len() as u32).to_be_bytes());
    for entry in &req.entries {
        buf.extend_from_slice(entry.owner.as_slice());
        buf.extend_from_slice(&(entry.cohort_blob.len() as u32).to_be_bytes());
        buf.extend_from_slice(&entry.cohort_blob);
    }
    alloy_primitives::keccak256(buf)
}

/// Domain-separated attestation preimage for a Fidelity snapshot batch.
pub fn fidelity_snapshot_attestation_preimage(
    inputs_canonical_hash: B256,
    leagues: &[FidelityLeagueEntry],
) -> Result<Vec<u8>, serde_json::Error> {
    let leagues_json = serde_json::to_vec(leagues)?;
    let mut buf = Vec::with_capacity(41 + 32 + 4 + leagues_json.len());
    buf.extend_from_slice(b"outbe/tee/fidelity-snapshot-attestation/v1");
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(leagues_json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&leagues_json);
    Ok(buf)
}

/// Deterministic hash over the canonical inputs of a single Fidelity index query.
#[must_use]
pub fn fidelity_query_canonical_hash(req: &FidelityQueryRequest) -> B256 {
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(req.chain_id.as_slice());
    buf.extend_from_slice(req.account.as_slice());
    buf.extend_from_slice(&(req.cohort_blob.len() as u32).to_be_bytes());
    buf.extend_from_slice(&req.cohort_blob);
    buf.extend_from_slice(&req.query_timestamp.to_be_bytes());
    buf.extend_from_slice(&req.block_timestamp.to_be_bytes());
    buf.extend_from_slice(&req.first_qualified_start.to_be_bytes());
    buf.extend_from_slice(&req.expiry.to_be_bytes());
    buf.extend_from_slice(&(req.owner_sig.len() as u32).to_be_bytes());
    buf.extend_from_slice(&req.owner_sig);
    alloy_primitives::keccak256(buf)
}

/// Domain-separated attestation preimage for a Fidelity index query.
pub fn fidelity_query_attestation_preimage(
    inputs_canonical_hash: B256,
    result: &FidelityQueryResult,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut probe = result.clone();
    probe.attestation_tag = Vec::new();
    let result_json = serde_json::to_vec(&probe)?;
    let mut buf = Vec::with_capacity(38 + 32 + 4 + result_json.len());
    buf.extend_from_slice(b"outbe/tee/fidelity-query-attestation/v1");
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(result_json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&result_json);
    Ok(buf)
}
