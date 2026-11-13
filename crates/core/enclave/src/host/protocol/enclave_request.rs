use alloy_primitives::{Address, U256};
use outbe_tee::{
    protocol::{EnclaveRequest as WireRequest, PayloadLane},
    CallMeta, TransportError,
};

use super::{
    EncryptedTributeOffer, FidelityCohortRequest, FidelityQueryRequest, FidelitySnapshotRequest,
    GratisOpRequest, Ledger, MineGratisFromPromisRequestV1, PromisOpRequest,
};

/// Domain requests carried inside [`outbe_tee::protocol::EnclaveRequest::Payload`].
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EnclaveRequest {
    ProcessTributeOfferBatch {
        offers: Vec<EncryptedTributeOffer>,
    },
    ApplyGratisOp {
        request: Box<GratisOpRequest>,
    },
    ApplyPromisOp {
        request: Box<PromisOpRequest>,
    },
    DeriveAccountKeys {
        ledger: Ledger,
        account: Address,
        requester_ephemeral_pubkey: [u8; 32],
        owner_sig: Vec<u8>,
    },
    ApplyFidelityCohortOp {
        request: Box<FidelityCohortRequest>,
    },
    SnapshotFidelityLeagues {
        request: Box<FidelitySnapshotRequest>,
    },
    QueryFidelityIndex {
        request: Box<FidelityQueryRequest>,
    },
    MineEncryptedNodV1 {
        request: Box<crate::host::nod_mine::MineEncryptedNodRequestV1>,
    },
    ReadNodAmountV1 {
        nod: outbe_core_common::nod_encryption::EncryptedNodV1,
    },
    EncryptNodForTributeV1 {
        tribute: Box<outbe_core_common::tribute_encryption::EncryptedTributeV2>,
        terms: outbe_core_common::nod_encryption::NodTermsV1,
        amount: U256,
    },
    ProcessEncryptedTributeOfferBatchV2 {
        offers: Vec<EncryptedTributeOffer>,
    },
    ReadTributeAmountsV2 {
        tributes: Vec<outbe_core_common::tribute_encryption::EncryptedTributeV2>,
    },
    ApplyTributeDayOpV2 {
        request: Box<crate::host::tribute_day::TributeDayOpRequestV2>,
    },
    ReadTributeDayAmountV2 {
        record: outbe_core_common::tribute_day_encryption::EncryptedTributeDayAmountV2,
    },
    ExecuteLysisFractionsV1 {
        request: Box<crate::host::tribute_day::LysisFractionsRequestV1>,
    },
    ReadTributeDayFractionsV1 {
        record: outbe_core_common::tribute_day_encryption::EncryptedTributeDayFractionsV1,
    },
    IssueNodFromTributeV1 {
        request: Box<crate::host::nod_mine::IssueNodFromTributeRequestV1>,
    },
    ApplyNodGroupLoadOpV1 {
        request: Box<crate::host::nod_mine::NodGroupLoadOpRequestV1>,
    },
    ReadNodGroupLoadV1 {
        record: outbe_core_common::nod_encryption::EncryptedNodGroupLoadV1,
    },
    EncryptNodQuoteV1 {
        nod: Box<outbe_core_common::nod_encryption::EncryptedNodV1>,
        context: outbe_core_common::nod_encryption::NodQuoteContextV1,
        payment_minor: U256,
    },
    MineGratisFromPromisV1 {
        request: Box<MineGratisFromPromisRequestV1>,
    },
}

impl EnclaveRequest {
    /// Stable snake_case label for metrics and logs on both host and enclave.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::ProcessTributeOfferBatch { .. } => "process_tribute_offer_batch",
            Self::ApplyGratisOp { .. } => "apply_gratis_op",
            Self::ApplyPromisOp { .. } => "apply_promis_op",
            Self::DeriveAccountKeys { .. } => "derive_account_keys",
            Self::ApplyFidelityCohortOp { .. } => "apply_fidelity_cohort_op",
            Self::SnapshotFidelityLeagues { .. } => "snapshot_fidelity_leagues",
            Self::QueryFidelityIndex { .. } => "query_fidelity_index",
            Self::MineEncryptedNodV1 { .. } => "mine_encrypted_nod_v1",
            Self::ReadNodAmountV1 { .. } => "read_nod_amount_v1",
            Self::EncryptNodForTributeV1 { .. } => "encrypt_nod_for_tribute_v1",
            Self::ProcessEncryptedTributeOfferBatchV2 { .. } => {
                "process_encrypted_tribute_offer_batch_v2"
            }
            Self::ReadTributeAmountsV2 { .. } => "read_tribute_amounts_v2",
            Self::ApplyTributeDayOpV2 { .. } => "apply_tribute_day_op_v2",
            Self::ReadTributeDayAmountV2 { .. } => "read_tribute_day_amount_v2",
            Self::ExecuteLysisFractionsV1 { .. } => "execute_lysis_fractions_v1",
            Self::ReadTributeDayFractionsV1 { .. } => "read_tribute_day_fractions_v1",
            Self::IssueNodFromTributeV1 { .. } => "issue_nod_from_tribute_v1",
            Self::ApplyNodGroupLoadOpV1 { .. } => "apply_nod_group_load_op_v1",
            Self::ReadNodGroupLoadV1 { .. } => "read_nod_group_load_v1",
            Self::EncryptNodQuoteV1 { .. } => "encrypt_nod_quote_v1",
            Self::MineGratisFromPromisV1 { .. } => "mine_gratis_from_promis_v1",
        }
    }

    /// Every domain request is pure or deterministic and safe to retry once on connection loss.
    #[must_use]
    pub const fn is_idempotent(&self) -> bool {
        true
    }

    /// True when the operation participates in the block's consensus witness tape.
    #[must_use]
    pub const fn is_recordable(&self) -> bool {
        !matches!(
            self,
            Self::ProcessTributeOfferBatch { .. }
                | Self::DeriveAccountKeys { .. }
                | Self::ApplyFidelityCohortOp { .. }
        )
    }

    /// Enforce strict transport-lane isolation on both host and enclave.
    #[must_use]
    pub const fn allows_lane(&self, lane: PayloadLane) -> bool {
        match self {
            Self::DeriveAccountKeys { .. } => matches!(lane, PayloadLane::AccountQuery),
            Self::ProcessTributeOfferBatch { .. }
            | Self::ProcessEncryptedTributeOfferBatchV2 { .. } => {
                matches!(lane, PayloadLane::Execution | PayloadLane::Canary)
            }
            _ => matches!(lane, PayloadLane::Execution),
        }
    }

    /// Build [`CallMeta`] for transport telemetry, retry policy, and witness recording.
    #[must_use]
    pub const fn call_meta(&self) -> CallMeta {
        CallMeta {
            label: self.label(),
            idempotent: self.is_idempotent(),
            recordable: self.is_recordable(),
        }
    }

    /// Encode this domain request into a lane-isolated [`WireRequest::Payload`].
    pub fn encode_wire(&self, lane: PayloadLane) -> Result<WireRequest, TransportError> {
        if !self.allows_lane(lane) {
            return Err(TransportError::EnclaveError(
                match lane {
                    PayloadLane::Execution => {
                        "request is not permitted on the execution connection"
                    }
                    PayloadLane::Canary => "request is not permitted on the canary connection",
                    PayloadLane::AccountQuery => {
                        "request is not permitted on the account-query connection"
                    }
                }
                .into(),
            ));
        }
        let payload = postcard::to_allocvec(self).map_err(TransportError::codec)?;
        Ok(WireRequest::Payload { lane, payload })
    }

    /// Strictly decode canonical `postcard` request bytes with zero trailing bytes.
    pub fn decode_wire_canonical(bytes: &[u8]) -> Result<Self, TransportError> {
        let (decoded, rest) =
            postcard::take_from_bytes::<Self>(bytes).map_err(TransportError::codec)?;
        if !rest.is_empty() {
            return Err(TransportError::Codec(
                "trailing bytes in enclave request payload".into(),
            ));
        }
        let reencoded = postcard::to_allocvec(&decoded).map_err(TransportError::codec)?;
        if reencoded != bytes {
            return Err(TransportError::Codec(
                "non-canonical enclave request payload".into(),
            ));
        }
        Ok(decoded)
    }
}

impl core::fmt::Debug for EnclaveRequest {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ExecuteLysisFractionsV1 { request } => f
                .debug_struct("ExecuteLysisFractionsV1")
                .field("chain_id", &request.chain_id)
                .field("worldwide_day", &request.worldwide_day)
                .field("emission_day", &request.emission_day)
                .field("completed_at", &request.completed_at)
                .field("metadosis_limit_minor", &request.metadosis_limit_minor)
                .field("tributes", &request.tributes.len())
                .finish(),
            Self::ReadTributeDayFractionsV1 { record } => f
                .debug_struct("ReadTributeDayFractionsV1")
                .field("chain_id", &record.chain_id)
                .field("worldwide_day", &record.worldwide_day)
                .field("emission_day", &record.emission_day)
                .field("completed_at", &record.completed_at)
                .field("encrypted_fractions", &"<redacted>")
                .finish(),
            Self::IssueNodFromTributeV1 { request } => f
                .debug_struct("IssueNodFromTributeV1")
                .field("nod_id", &request.terms.nod_id)
                .field("owner", &request.terms.owner)
                .field("tribute", &"<redacted>")
                .field("day_fractions", &"<redacted>")
                .finish(),
            Self::ApplyNodGroupLoadOpV1 { request } => f
                .debug_struct("ApplyNodGroupLoadOpV1")
                .field("chain_id", &request.chain_id)
                .field("scope", &request.scope)
                .field("operation", &"<redacted>")
                .finish(),
            Self::ReadNodGroupLoadV1 { record } => f
                .debug_struct("ReadNodGroupLoadV1")
                .field("chain_id", &record.chain_id)
                .field("scope", &record.scope)
                .field("encrypted_load", &"<redacted>")
                .finish(),
            Self::EncryptNodQuoteV1 { context, .. } => f
                .debug_struct("EncryptNodQuoteV1")
                .field("nod_id", &context.nod_id)
                .field("owner", &context.owner)
                .field("asset", &context.asset)
                .field("payment_minor", &"<redacted>")
                .finish(),
            Self::MineGratisFromPromisV1 { request } => f
                .debug_struct("MineGratisFromPromisV1")
                .field("chain_id", &request.chain_id)
                .field("account", &request.account)
                .field("encrypted_promis_amount", &"<redacted>")
                .finish(),
            other => f
                .debug_struct("EnclaveRequest")
                .field("label", &other.label())
                .finish_non_exhaustive(),
        }
    }
}
