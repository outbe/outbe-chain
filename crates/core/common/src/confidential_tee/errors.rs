//! Typed errors for shared confidential cryptography and host clients.
//!
//! Wrapping another error uses `#[source]`. These types do not render a cause
//! with `to_string` or `format!("{err}")`.

use alloy_primitives::B256;
use outbe_tee::{AppErrorDescriptor, TeeAttestationError, TransportError};
use thiserror::Error;

/// Define the shared fatal variants while retaining each domain's error type and extra variants.
#[macro_export]
macro_rules! define_confidential_fatal_error {
    ($(#[$doc:meta])* $name:ident, $op_text:ident, $attestation:literal; $($extra:tt)*) => {
        $(#[$doc])*
        #[derive(Debug, ::thiserror::Error)]
        #[non_exhaustive]
        pub enum $name {
            /// No in-process runtime is installed.
            #[error("tee_sidecar_unavailable: {context}")]
            SidecarUnavailable {
                /// Operation label.
                context: &'static str,
            },
            /// The runtime returned an unclassified transport failure.
            #[error("tee_sidecar_unavailable: {context}: {source}")]
            SidecarRequestFailed {
                /// Operation label.
                context: &'static str,
                /// Transport failure.
                #[source]
                source: ::outbe_tee::TransportError,
            },
            /// The enclave rejected the operation.
            #[error("enclave {context} error: {message}")]
            EnclaveOpError {
                /// Operation label.
                context: &'static str,
                /// Inner enclave sentence. The transport error is its source.
                #[source]
                message: $op_text,
            },
            /// The echoed inputs hash does not match the host's canonical hash.
            #[error("tee_enclave_nondeterminism")]
            EnclaveNondeterminism,
            /// The Ed25519 attestation tag did not verify.
            #[error($attestation)]
            AttestationInvalid {
                /// Attestation failure.
                #[source]
                source: ::outbe_tee::TransportError,
            },
            $($extra)*
        }

        impl $name {
            /// Classify a recorded witness transport failure onto the historical domain sentences.
            #[allow(dead_code)]
            pub(crate) fn from_consensus_transport(
                operation: &'static str,
                error: ::outbe_tee::TransportError,
            ) -> Self {
                $crate::confidential_tee::map_consensus_transport_error(
                    error,
                    Self::EnclaveNondeterminism,
                    |source| Self::EnclaveOpError {
                        context: operation,
                        message: $op_text(source),
                    },
                    |source| Self::AttestationInvalid { source },
                    |source| Self::SidecarRequestFailed {
                        context: operation,
                        source,
                    },
                )
            }
        }
    };
}

/// Classify a recorded operation's transport failure before adding domain context.
pub fn map_consensus_transport_error<E>(
    error: TransportError,
    mismatch: E,
    rejected: impl FnOnce(TransportError) -> E,
    invalid_attestation: impl FnOnce(TransportError) -> E,
    unavailable: impl FnOnce(TransportError) -> E,
) -> E {
    match error {
        TransportError::UnexpectedResponse => mismatch,
        TransportError::EnclaveError(_) | TransportError::EnclaveCause { .. } => rejected(error),
        TransportError::AppAttestation { .. } => invalid_attestation(error),
        other => unavailable(other),
    }
}

/// Tribute offer, read, day, or NOD attestation failure descriptor.
pub const TRIBUTE_OFFER_ATTESTATION: AppErrorDescriptor = AppErrorDescriptor::new(
    "tribute_offer_attestation",
    "offer attestation signature invalid",
);

/// Gratis ledger operation attestation failure descriptor.
pub const GRATIS_OP_ATTESTATION: AppErrorDescriptor = AppErrorDescriptor::new(
    "gratis_op_attestation",
    "gratis-op attestation signature invalid",
);

/// Promis ledger operation attestation failure descriptor.
pub const PROMIS_OP_ATTESTATION: AppErrorDescriptor = AppErrorDescriptor::new(
    "promis_op_attestation",
    "promis-op attestation signature invalid",
);

/// Fidelity operation attestation failure descriptor.
pub const FIDELITY_ATTESTATION: AppErrorDescriptor = AppErrorDescriptor::new(
    "fidelity_attestation",
    "fidelity attestation signature invalid",
);

/// Tribute state-source proof or context rejection descriptor.
pub const TRIBUTE_SOURCE_REJECTED: AppErrorDescriptor =
    AppErrorDescriptor::new("tribute_source_rejected", "Tribute source rejected");

/// Encrypted NOD mint business rejection descriptor.
pub const NOD_MINT_REJECTED: AppErrorDescriptor =
    AppErrorDescriptor::new("nod_mint_rejected", "encrypted NOD mint rejected");

/// Error from shared enclave-domain cryptography.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TeeError {
    /// Resident handle rejected the operation. Display text stays the historical
    /// AEAD/HKDF sentence; the resident error remains the source.
    #[error("{}", resident_error_message(.0))]
    Resident(#[from] outbe_tee::resident::TeeCryptoError),

    /// The nonce size is not 12 bytes.
    #[error("invalid nonce size: expected 12 bytes, got {0}")]
    InvalidNonce(usize),

    /// AEAD encryption failed before a resident error was available.
    ///
    /// `None` is a structural refusal, such as a version that does not fit in `u64`.
    /// `Some` is the `ring` failure that produced the same display sentence.
    #[error("AEAD encryption failed")]
    EncryptFailed(#[source] Option<ring::error::Unspecified>),

    /// AEAD decryption failed before a resident error was available.
    ///
    /// `None` is a structural refusal, such as a short envelope. `Some` is the
    /// `ring` failure that produced the same display sentence.
    #[error("AEAD decryption failed (bad key, nonce, or tampered ciphertext)")]
    DecryptFailed(#[source] Option<ring::error::Unspecified>),

    /// HKDF-SHA256 key derivation failed before a resident error was available.
    #[error("HKDF-SHA256 derivation failed")]
    HkdfFailed(#[source] ring::error::Unspecified),

    /// A fixed onboarding test fixture names a different offer key.
    #[cfg(any(test, feature = "dev-keys"))]
    #[error("DKG session error: onboarding context offer public does not match resident key")]
    OnboardingFixtureOfferKeyMismatch,
}

fn resident_error_message(error: &outbe_tee::resident::TeeCryptoError) -> &'static str {
    use outbe_tee::{errors::TeeCryptoError as Primitive, resident::TeeCryptoError as Resident};
    match error {
        Resident::InvalidEnvelope
        | Resident::Primitive(Primitive::InvalidEnvelope | Primitive::DecryptionFailed) => {
            "AEAD decryption failed (bad key, nonce, or tampered ciphertext)"
        }
        Resident::Primitive(Primitive::HkdfExpand | Primitive::HkdfFill) => {
            "HKDF-SHA256 derivation failed"
        }
        Resident::Primitive(Primitive::AeadSeal) => "AEAD encryption failed",
        _ => "resident cryptographic operation failed",
    }
}

/// Hex decoder failure for a `tribute_draft_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DraftIdHexError {
    /// A character was not a hex digit.
    #[error("invalid hex character {c:?} at index {index}")]
    InvalidHexCharacter {
        /// Rejected character.
        c: char,
        /// Byte index of that character in the stripped hex text.
        index: usize,
    },
    /// The stripped hex text had an odd number of digits.
    #[error("odd hex length")]
    OddLength,
    /// The hex crate rejected the string length.
    #[error("invalid hex string length")]
    InvalidStringLength,
}

/// Error from canonical amount parsing and nominal math shared by offer payloads.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ComputeError {
    /// The `tribute_draft_id` hex string failed hex decoding.
    #[error("invalid tribute_draft_id hex: {draft_id}")]
    InvalidDraftIdHex {
        /// Caller text that failed decoding, including any `0x` prefix.
        draft_id: String,
        /// Structured decoder failure. Display keeps the historical sentence.
        #[source]
        source: DraftIdHexError,
    },
    /// The decoded `tribute_draft_id` length is not 32 bytes.
    #[error("tribute_draft_id must be 32 bytes, got {len}")]
    DraftIdLength { len: usize },
    /// The issuance WorldwideDay VWAP is zero.
    #[error("issuance WorldwideDay VWAP is zero")]
    IssuanceVwapZero,
    /// The reference WorldwideDay VWAP is zero.
    #[error("reference WorldwideDay VWAP is zero")]
    ReferenceVwapZero,
    /// The nominal amount calculation overflowed.
    #[error("nominal amount overflow")]
    NominalAmountOverflow,
    /// The nominal denominator multiplication overflowed.
    #[error("nominal denominator overflow")]
    NominalDenominatorOverflow,
    /// A positive amount rounded down to zero nominal units.
    #[error("nominal amount rounds to zero")]
    NominalRoundsToZero,
    /// A numeric string is not a canonical u64 representation.
    #[error("{field} must be a canonical u64")]
    NonCanonicalU64 { field: &'static str },
    /// The `amount_micro` field is greater than or equal to 1,000,000.
    #[error("amount_micro must be less than 1000000")]
    AmountMicroRange,
    /// Combining base and micro amounts overflowed.
    #[error("amount overflow")]
    AmountOverflow,
}

/// Typed rejection causes for a Tribute state-source opening.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum TributeSourceError {
    /// Checkpoint or Tribute context identity failed structural validation.
    #[error("Tribute source context mismatch")]
    ContextMismatch,
    /// Expected storage slot list is empty, unsorted, oversized, or misaligned.
    #[error("Tribute source slot set mismatch")]
    SlotSetMismatch,
    /// Source binding hash differed from the node's expected authority binding.
    #[error("Tribute source does not match node authority")]
    AuthorityMismatch,
    /// Proven storage word differed from the expected word value.
    #[error("Tribute source word mismatch")]
    WordMismatch,
    /// Underlying MPT account/storage proof verification failed.
    #[error(transparent)]
    ProofVerification(#[from] outbe_primitives::storage::proof::StateProofError),
}

/// Typed host-client errors for domain enclave operations.
#[derive(Debug, Error)]
pub enum EnclaveClientError {
    /// Enclave deterministically rejected an encrypted NOD mint operation.
    #[error("encrypted NOD mint rejected: {reason}")]
    NodMintRejected {
        /// Enclave-signed rejection reason text.
        reason: String,
    },
    /// Tribute state source verification rejected the supplied opening.
    #[error("Tribute source rejected: {0}")]
    TributeSourceRejected(#[from] TributeSourceError),
    /// Tribute day or league predecessor record carried an invalid version.
    #[error("invalid Tribute day predecessor version: expected {expected}, got {actual}")]
    InvalidTributeDayPredecessorVersion {
        /// Minimum expected predecessor version.
        expected: u8,
        /// Actual predecessor version byte observed.
        actual: u8,
    },
    /// Tribute offer or Tribute V2 attestation signature check failed.
    #[error("offer attestation signature invalid: {0}")]
    TributeOfferAttestation(#[source] TeeAttestationError),
    /// Tribute day or league attestation signature check failed.
    #[error("tribute-day attestation signature invalid: {0}")]
    TributeDayAttestation(#[source] TeeAttestationError),
    /// Gratis ledger operation attestation signature check failed.
    #[error("gratis-op attestation signature invalid: {0}")]
    GratisOpAttestation(#[source] TeeAttestationError),
    /// Promis ledger operation attestation signature check failed.
    #[error("promis-op attestation signature invalid: {0}")]
    PromisOpAttestation(#[source] TeeAttestationError),
    /// Fidelity operation attestation signature check failed.
    #[error("fidelity attestation signature invalid: {0}")]
    FidelityAttestation(#[source] TeeAttestationError),
    /// NOD operation attestation signature check failed.
    #[error("nod attestation signature invalid: {0}")]
    NodAttestation(#[source] TeeAttestationError),
    /// Underlying enclave transport or codec error.
    #[error(transparent)]
    Transport(#[from] TransportError),
}

/// Errors returned by a confidential balance witness call.
#[derive(Debug, Error)]
pub enum BalanceClientError {
    /// Request variant is not a Gratis or Promis balance operation.
    #[error("request is not a confidential balance operation")]
    NotConfidentialBalanceOperation,
    /// The resident enclave failed with a node-local or enclave-state error.
    #[error("tee_sidecar_unavailable: {0}")]
    SidecarRequestUnavailable(#[source] TransportError),
    /// Enclave request failed with a deterministic protocol error.
    #[error("enclave request failed: {0}")]
    RequestFailed(#[source] TransportError),
    /// Gratis attestation signature check failed.
    #[error("tee_gratis_attestation_invalid: {0}")]
    GratisAttestationInvalid(#[source] TransportError),
    /// Promis attestation signature check failed.
    #[error("tee_promis_attestation_invalid: {0}")]
    PromisAttestationInvalid(#[source] TransportError),
    /// Enclave returned an explicit operation error string from the protocol.
    #[error("enclave {operation} error: {message}")]
    EnclaveOperationError {
        /// Name of the balance operation.
        operation: &'static str,
        /// Protocol error text from the enclave response.
        message: String,
    },
    /// Enclave returned a response variant that does not match the request.
    #[error("unexpected enclave response tag {tag}")]
    UnexpectedResponse {
        /// Frozen V1 response tag byte, or `0xFF` when the payload had no tag.
        tag: u8,
    },
    /// Enclave echoed a canonical input hash that differs from the request hash.
    #[error("tee_enclave_nondeterminism")]
    EnclaveNondeterminism,
}

impl BalanceClientError {
    /// Classify a transport failure while preserving the structured cause.
    #[must_use]
    pub fn from_request_failure(error: TransportError) -> Self {
        if error.is_node_local() || matches!(error, TransportError::EnclaveError(_)) {
            Self::SidecarRequestUnavailable(error)
        } else {
            Self::RequestFailed(error)
        }
    }
}

/// Errors returned by local encryption and owner-local opening helpers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum LocalCryptoError {
    /// System random number generator failed during offer encryption.
    #[error("offer encryption rng failure")]
    OfferEncryptionRngFailure(#[source] outbe_tee::TeeCryptoError),
    /// HKDF-SHA256 expand step failed.
    #[error("hkdf expand")]
    HkdfExpand(#[source] outbe_tee::TeeCryptoError),
    /// HKDF-SHA256 output fill step failed.
    #[error("hkdf fill")]
    HkdfFill(#[source] outbe_tee::TeeCryptoError),
    /// ChaCha20Poly1305 key initialization failed.
    #[error("aead key")]
    AeadKey(#[source] outbe_tee::TeeCryptoError),
    /// ChaCha20Poly1305 in-place sealing failed.
    #[error("aead seal")]
    AeadSeal(#[source] outbe_tee::TeeCryptoError),
    /// Sealed offer bytes did not have the fixed wire length.
    #[error("aead seal")]
    AeadOutputLength,
    /// Encrypted Fidelity cohort blob failed envelope validation.
    #[error("invalid encrypted Fidelity cohort envelope")]
    InvalidFidelityCohortEnvelope,
    /// Fidelity cohort ciphertext failed AEAD.
    #[error("Fidelity cohort decryption failed")]
    FidelityCohortDecryptionFailed(#[source] outbe_tee::TeeCryptoError),
    /// Encrypted Gratis balance blob failed envelope validation.
    #[error("invalid encrypted Gratis balance")]
    InvalidGratisBalance,
    /// Gratis balance ciphertext failed AEAD.
    #[error("Gratis balance decryption failed")]
    GratisBalanceDecryptionFailed(#[source] outbe_tee::TeeCryptoError),
    /// Opened Gratis plaintext did not have the 32-byte U256 length.
    #[error("invalid Gratis amount length")]
    InvalidGratisAmountLength(#[source] outbe_tee::TeeCryptoError),
    /// Encrypted Promis balance blob failed envelope validation.
    #[error("invalid encrypted Promis balance")]
    InvalidPromisBalance,
    /// Promis balance ciphertext failed AEAD.
    #[error("Promis balance decryption failed")]
    PromisBalanceDecryptionFailed(#[source] outbe_tee::TeeCryptoError),
    /// Opened Promis plaintext did not have the 32-byte U256 length.
    #[error("invalid Promis amount length")]
    InvalidPromisAmountLength(#[source] outbe_tee::TeeCryptoError),
    /// Encrypted NOD record failed structural encoding validation.
    #[error("invalid encrypted NOD encoding")]
    InvalidNodEncoding,
    /// Encrypted NOD amount failed AEAD or key derivation.
    #[error("NOD amount decryption failed")]
    NodAmountDecryptionFailed(#[source] outbe_tee::TeeCryptoError),
    /// Opened NOD plaintext did not have the 32-byte U256 length.
    #[error("invalid NOD amount length")]
    InvalidNodAmountLength(#[source] outbe_tee::TeeCryptoError),
    /// Derived NOD amount key failed AEAD key initialization.
    #[error("invalid NOD amount key")]
    InvalidNodAmountKey(#[source] outbe_tee::TeeCryptoError),
    /// Encrypted Tribute record failed structural encoding validation.
    #[error("invalid encrypted Tribute encoding")]
    InvalidTributeEncoding,
    /// Encrypted Tribute amounts failed AEAD or key derivation.
    #[error("Tribute amount decryption failed")]
    TributeAmountDecryptionFailed(#[source] outbe_tee::TeeCryptoError),
    /// Opened Tribute plaintext did not have the 64-byte amounts length.
    #[error("invalid Tribute amount length")]
    InvalidTributeAmountLength(#[source] outbe_tee::TeeCryptoError),
    /// Derived Tribute amount key failed AEAD key initialization.
    #[error("invalid Tribute amount key")]
    InvalidTributeAmountKey(#[source] outbe_tee::TeeCryptoError),
}

impl PartialEq<&str> for LocalCryptoError {
    fn eq(&self, other: &&str) -> bool {
        self.static_message() == *other
    }
}

impl PartialEq<str> for LocalCryptoError {
    fn eq(&self, other: &str) -> bool {
        self.static_message() == other
    }
}

impl LocalCryptoError {
    fn static_message(&self) -> &'static str {
        match self {
            Self::OfferEncryptionRngFailure(_) => "offer encryption rng failure",
            Self::HkdfExpand(_) => "hkdf expand",
            Self::HkdfFill(_) => "hkdf fill",
            Self::AeadKey(_) => "aead key",
            Self::AeadSeal(_) | Self::AeadOutputLength => "aead seal",
            Self::InvalidFidelityCohortEnvelope => "invalid encrypted Fidelity cohort envelope",
            Self::FidelityCohortDecryptionFailed(_) => "Fidelity cohort decryption failed",
            Self::InvalidGratisBalance => "invalid encrypted Gratis balance",
            Self::GratisBalanceDecryptionFailed(_) => "Gratis balance decryption failed",
            Self::InvalidGratisAmountLength(_) => "invalid Gratis amount length",
            Self::InvalidPromisBalance => "invalid encrypted Promis balance",
            Self::PromisBalanceDecryptionFailed(_) => "Promis balance decryption failed",
            Self::InvalidPromisAmountLength(_) => "invalid Promis amount length",
            Self::InvalidNodEncoding => "invalid encrypted NOD encoding",
            Self::NodAmountDecryptionFailed(_) => "NOD amount decryption failed",
            Self::InvalidNodAmountLength(_) => "invalid NOD amount length",
            Self::InvalidNodAmountKey(_) => "invalid NOD amount key",
            Self::InvalidTributeEncoding => "invalid encrypted Tribute encoding",
            Self::TributeAmountDecryptionFailed(_) => "Tribute amount decryption failed",
            Self::InvalidTributeAmountLength(_) => "invalid Tribute amount length",
            Self::InvalidTributeAmountKey(_) => "invalid Tribute amount key",
        }
    }
}

/// Decode a 32-byte `tribute_draft_id`. An optional `0x` prefix is accepted.
///
/// The decoded bytes are validated and then discarded by token-id derivation.
/// A malformed id still fails this function.
pub fn parse_tribute_draft_id(draft_id: &str) -> Result<[u8; 32], ComputeError> {
    let hex_str = draft_id.strip_prefix("0x").unwrap_or(draft_id);
    let bytes = hex::decode(hex_str).map_err(|source| ComputeError::InvalidDraftIdHex {
        draft_id: draft_id.to_owned(),
        source: draft_id_hex_error(source),
    })?;
    if bytes.len() != 32 {
        return Err(ComputeError::DraftIdLength { len: bytes.len() });
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn draft_id_hex_error(error: hex::FromHexError) -> DraftIdHexError {
    match error {
        hex::FromHexError::InvalidHexCharacter { c, index } => {
            DraftIdHexError::InvalidHexCharacter { c, index }
        }
        hex::FromHexError::OddLength => DraftIdHexError::OddLength,
        hex::FromHexError::InvalidStringLength => DraftIdHexError::InvalidStringLength,
    }
}

pub(crate) fn map_offer_encrypt(error: outbe_tee::TeeCryptoError) -> LocalCryptoError {
    use outbe_tee::TeeCryptoError as E;
    match error {
        E::RngFailure => LocalCryptoError::OfferEncryptionRngFailure(error),
        E::HkdfExpand => LocalCryptoError::HkdfExpand(error),
        E::HkdfFill => LocalCryptoError::HkdfFill(error),
        E::AeadKey => LocalCryptoError::AeadKey(error),
        E::AeadSeal
        | E::InvalidEnvelope
        | E::DecryptionFailed
        | E::InvalidPlaintextLength { .. } => LocalCryptoError::AeadSeal(error),
    }
}

pub(crate) fn inputs_hash_mismatch(expected: B256, actual: B256) -> TeeAttestationError {
    TeeAttestationError::InputsHashMismatch { expected, actual }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::*;

    #[test]
    fn balance_request_failure_keeps_the_transport_source() {
        let cause = TransportError::UnexpectedResponse;
        let error = BalanceClientError::from_request_failure(cause);
        let BalanceClientError::RequestFailed(source) = error else {
            panic!("deterministic transport failure is RequestFailed");
        };
        assert!(source.source().is_none());
        assert!(matches!(source, TransportError::UnexpectedResponse));

        let local = TransportError::Unavailable("down".into());
        let error = BalanceClientError::from_request_failure(local);
        let BalanceClientError::SidecarRequestUnavailable(source) = error else {
            panic!("node-local failure stays sidecar-unavailable");
        };
        assert!(matches!(source, TransportError::Unavailable(_)));
    }

    #[test]
    fn attestation_client_error_downcasts_to_the_signature_cause() {
        let cause = TeeAttestationError::InvalidSignatureLength { actual: 3 };
        let error = EnclaveClientError::GratisOpAttestation(cause);
        let source = error.source().expect("attestation source");
        assert!(matches!(
            source.downcast_ref::<TeeAttestationError>(),
            Some(TeeAttestationError::InvalidSignatureLength { actual: 3 })
        ));
    }

    #[test]
    fn compute_error_is_a_leaf() {
        let error = ComputeError::IssuanceVwapZero;
        assert!(error.source().is_none());
        assert_eq!(error.to_string(), "issuance WorldwideDay VWAP is zero");
    }

    #[test]
    fn draft_id_hex_failure_keeps_the_decoder() {
        let error = parse_tribute_draft_id("0xzz").unwrap_err();
        let ComputeError::InvalidDraftIdHex { draft_id, source } = error else {
            panic!("odd or non-hex draft id is InvalidDraftIdHex");
        };
        assert_eq!(draft_id, "0xzz");
        assert!(matches!(
            source,
            DraftIdHexError::InvalidHexCharacter { c: 'z', index: 0 }
        ));
        assert_eq!(
            parse_tribute_draft_id(&format!("0x{}", "11".repeat(32))).unwrap(),
            [0x11; 32]
        );
        assert!(matches!(
            parse_tribute_draft_id("0x11").unwrap_err(),
            ComputeError::DraftIdLength { len: 1 }
        ));
    }
}
