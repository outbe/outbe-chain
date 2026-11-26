//! Frozen V1 domain response tags on the consensus witness tape.
//!
//! [`outbe_tee::witness::EnclaveWitnessEntryV1::Payload`] stores three fields,
//! byte-identical to the resident wire layout. `payload` is `tag || body`.
//! `body` is the canonical tag-free postcard of the response. The inner
//! `attestation_tag` field is empty (`0x00`) and the inner
//! `inputs_canonical_hash` equals the entry's own hash. The Ed25519 signature
//! lives only in the entry's `attestation_tag`. The tags of [`NON_RECORDABLE_V1_TAGS`]
//! never enter the tape. `MineEncryptedNodV1` uses tag `9` or `8`.
//! Those tags are different postcard schemas. The outer tag selects the schema.
//! The body has no second discriminant.

use alloy_primitives::{Address, B256};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::resident::StateKeyHandle;
use outbe_tee::witness::TypedWitnessPayload;
use outbe_tee::{tee_consensus_typed_call, tee_session_typed_call, CallMeta, TransportError};
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;

use super::confidential::{domain_for, Domain, Ledger};

/// Implement the frozen witness tag and metadata accessors for an existing response body.
#[macro_export]
macro_rules! impl_confidential_witness_schema {
    ($body:ty, $tag:ident) => {
        impl $crate::confidential_tee::WitnessSchema for $body {
            const TAG: $crate::confidential_tee::WitnessResponseTag =
                $crate::confidential_tee::WitnessResponseTag::$tag;

            fn inputs_canonical_hash(&self) -> ::alloy_primitives::B256 {
                self.inputs_canonical_hash
            }

            fn attestation_tag(&self) -> &[u8] {
                &self.attestation_tag
            }

            fn set_attestation_tag(&mut self, tag: Vec<u8>) {
                self.attestation_tag = tag;
            }
        }
    };
}

/// Highest frozen V1 domain response discriminant.
pub const V1_RESPONSE_TAG_MAX: u8 = 22;

/// Domain response discriminants that must not be written to the witness tape.
pub const NON_RECORDABLE_V1_TAGS: [u8; 14] = [0, 3, 4, 5, 6, 7, 11, 12, 13, 14, 15, 16, 17, 18];

/// Recordable frozen V1 domain response tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum WitnessResponseTag {
    /// `GratisOpApplied`.
    GratisOpApplied = 1,
    /// `PromisOpApplied`.
    PromisOpApplied = 2,
    /// `EncryptedNodMintRejectedV1`.
    EncryptedNodMintRejectedV1 = 8,
    /// `EncryptedNodMinedV1`.
    EncryptedNodMinedV1 = 9,
    /// `NodAmountReadV1`.
    NodAmountReadV1 = 10,
    /// `NodGroupLoadOpAppliedV1`.
    NodGroupLoadOpAppliedV1 = 19,
    /// `NodGroupLoadReadV1`.
    NodGroupLoadReadV1 = 20,
    /// `NodQuoteEncryptedV1`.
    NodQuoteEncryptedV1 = 21,
    /// `NodIssuedAtOfferV1`.
    NodIssuedAtOfferV1 = 22,
}

impl WitnessResponseTag {
    /// Every recordable V1 tag, in discriminant order.
    pub const ALL: [Self; 9] = [
        Self::GratisOpApplied,
        Self::PromisOpApplied,
        Self::EncryptedNodMintRejectedV1,
        Self::EncryptedNodMinedV1,
        Self::NodAmountReadV1,
        Self::NodGroupLoadOpAppliedV1,
        Self::NodGroupLoadReadV1,
        Self::NodQuoteEncryptedV1,
        Self::NodIssuedAtOfferV1,
    ];

    /// Frozen postcard discriminant.
    #[must_use]
    pub const fn value(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for WitnessResponseTag {
    type Error = WitnessTagError;

    fn try_from(tag: u8) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.value() == tag)
            .ok_or_else(|| WitnessTagError::rejected(tag))
    }
}

/// A V1 response tag cannot be placed on the consensus witness tape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WitnessTagError {
    /// Tag is outside `0..=22`.
    #[error("witness response tag {tag} is outside the frozen V1 range 0..={V1_RESPONSE_TAG_MAX}")]
    OutOfRange {
        /// Observed tag byte.
        tag: u8,
    },
    /// Tag is one of [`NON_RECORDABLE_V1_TAGS`].
    #[error("witness response tag {tag} is not recordable")]
    NonRecordable {
        /// Observed tag byte.
        tag: u8,
    },
    /// Payload tag differs from the tag the caller committed to.
    #[error("witness payload tag {actual} does not match expected {expected}")]
    Mismatch {
        /// Tag the caller asked to decode.
        expected: u8,
        /// Tag byte stored in the payload.
        actual: u8,
    },
    /// Payload did not contain a tag byte.
    #[error("witness payload is missing the V1 response tag")]
    MissingTag,
    /// Postcard decoded a value and left unused bytes.
    #[error("witness payload has trailing bytes after the canonical body")]
    TrailingBytes,
    /// Postcard decoded a value whose canonical encoding differs from the body.
    #[error("witness payload body is not canonically encoded")]
    NonCanonical,
    /// Decoded body kept a non-empty `attestation_tag`.
    #[error("witness payload carries a non-empty inner attestation tag")]
    InnerAttestationTag,
    /// Body `inputs_canonical_hash` differs from the witness entry field.
    #[error("witness payload inputs hash does not match the witness entry")]
    InputsHashMismatch,
}

impl WitnessTagError {
    fn rejected(tag: u8) -> Self {
        if NON_RECORDABLE_V1_TAGS.contains(&tag) {
            Self::NonRecordable { tag }
        } else {
            Self::OutOfRange { tag }
        }
    }
}

/// One encoded witness response. The tag is chosen here, including the pair
/// `MineEncryptedNodV1` `9|8`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaggedWitnessParts {
    /// Recordable V1 response tag.
    pub tag: WitnessResponseTag,
    /// Canonical tag-free postcard body.
    pub body: Vec<u8>,
    /// Canonical digest of the operation inputs.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature of the block leader or the local verifier.
    pub attestation_tag: Vec<u8>,
}

/// Borrowed [`TaggedWitnessParts`] after the tag byte has been split off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaggedWitnessRef<'a> {
    /// Tag byte taken from the front of `payload`.
    pub tag: WitnessResponseTag,
    /// Canonical tag-free postcard body.
    pub body: &'a [u8],
    /// Canonical digest of the operation inputs.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature stored beside the payload.
    pub attestation_tag: &'a [u8],
}

/// Encode `value` as canonical postcard with an empty remainder on decode.
pub fn encode_canonical_body<B: Serialize>(value: &B) -> Result<Vec<u8>, TransportError> {
    postcard::to_allocvec(value).map_err(TransportError::codec)
}

/// Decode one canonical postcard value.
///
/// Trailing bytes and an encoding that does not round-trip through
/// [`postcard::to_allocvec`] fail closed.
pub fn decode_canonical_body<B: DeserializeOwned + Serialize>(
    body: &[u8],
) -> Result<B, TransportError> {
    let (value, rest) = postcard::take_from_bytes::<B>(body).map_err(TransportError::codec)?;
    if !rest.is_empty() {
        return Err(TransportError::codec(WitnessTagError::TrailingBytes));
    }
    let reencoded = postcard::to_allocvec(&value).map_err(TransportError::codec)?;
    if reencoded != body {
        return Err(TransportError::codec(WitnessTagError::NonCanonical));
    }
    Ok(value)
}

/// Encode `value` and prefix the frozen V1 tag.
pub fn encode_tagged_payload<T: Serialize>(
    tag: WitnessResponseTag,
    value: &T,
) -> Result<Vec<u8>, TransportError> {
    Ok(prefix_tag(tag, encode_canonical_body(value)?))
}

/// Decode a payload that starts with `tag`.
///
/// The body must be one canonical postcard value. Trailing bytes and an
/// encoding that does not round-trip through `postcard::to_allocvec` fail closed.
pub fn decode_tagged_payload<T: DeserializeOwned + Serialize>(
    tag: WitnessResponseTag,
    bytes: &[u8],
) -> Result<T, TransportError> {
    let body = strip_tag(tag, bytes).map_err(TransportError::codec)?;
    decode_canonical_body(body)
}

/// Postcard encoder for one witness body.
///
/// The byte buffer is private. [`Self::encode`] is the only write path.
pub struct CanonicalBodyEncoder(());

impl CanonicalBodyEncoder {
    /// Write `body` as canonical postcard.
    pub fn encode<B: Serialize>(self, body: &B) -> Result<Vec<u8>, TransportError> {
        encode_canonical_body(body)
    }
}

/// Postcard decoder for one witness body.
///
/// The borrowed bytes are private. [`Self::decode`] consumes `self`, so a
/// [`WitnessDomainWire`] implementation cannot inspect or slice the payload.
pub struct CanonicalBodyDecoder<'a>(&'a [u8]);

impl<'a> CanonicalBodyDecoder<'a> {
    /// Read one canonical postcard value.
    ///
    /// Trailing bytes and a value that does not round-trip through
    /// [`postcard::to_allocvec`] fail closed.
    pub fn decode<B: DeserializeOwned + Serialize>(self) -> Result<B, TransportError> {
        decode_canonical_body(self.0)
    }
}

/// One postcard schema identified by a single V1 tag.
///
/// `Self` is the body. Applied and rejected answers are two schemas, not two
/// variants of one postcard enum.
pub trait WitnessSchema: Serialize + DeserializeOwned + Clone {
    /// V1 tag stored in front of this body.
    const TAG: WitnessResponseTag;
    /// Inner inputs digest. It must equal the witness entry field.
    fn inputs_canonical_hash(&self) -> B256;
    /// Inner Ed25519 signature. The tape copy of this field is empty.
    fn attestation_tag(&self) -> &[u8];
    /// Replace the inner signature. Encoding passes an empty vector.
    fn set_attestation_tag(&mut self, tag: Vec<u8>);
}

mod sealed {
    pub trait Sealed {}
}

impl<T: WitnessSchema> sealed::Sealed for T {}

/// Domain response stored on the witness tape.
///
/// External crates implement [`WitnessSchema`]. This trait is sealed, so they
/// cannot supply a `decode_wire` that skips [`CanonicalBodyDecoder`].
/// [`Self::decode_wire`] sees the outer V1 tag before it reads the body.
/// [`DualWitnessAnswer`] uses that tag to pick one of two schemas.
#[allow(private_bounds)]
pub trait WitnessDomainWire: Sized + Clone + sealed::Sealed {
    /// Inner inputs digest. It must equal the witness entry field.
    fn inputs_canonical_hash(&self) -> B256;
    /// Inner Ed25519 signature. The tape copy of this field is empty.
    fn attestation_tag(&self) -> &[u8];
    /// Replace the inner signature. Encoding passes an empty vector.
    fn set_attestation_tag(&mut self, tag: Vec<u8>);
    /// Encode the tag-free body of the active schema.
    ///
    /// The inner `attestation_tag` is cleared first. A clear that leaves the
    /// field non-empty fails closed.
    fn encode_wire(
        &self,
        encoder: CanonicalBodyEncoder,
    ) -> Result<(WitnessResponseTag, Vec<u8>), TransportError>;
    /// Decode the schema selected by `tag`.
    fn decode_wire(
        tag: WitnessResponseTag,
        decoder: CanonicalBodyDecoder<'_>,
    ) -> Result<Self, TransportError>;
}

impl<T: WitnessSchema> WitnessDomainWire for T {
    fn inputs_canonical_hash(&self) -> B256 {
        WitnessSchema::inputs_canonical_hash(self)
    }

    fn attestation_tag(&self) -> &[u8] {
        WitnessSchema::attestation_tag(self)
    }

    fn set_attestation_tag(&mut self, tag: Vec<u8>) {
        WitnessSchema::set_attestation_tag(self, tag);
    }

    fn encode_wire(
        &self,
        encoder: CanonicalBodyEncoder,
    ) -> Result<(WitnessResponseTag, Vec<u8>), TransportError> {
        let mut cleared = self.clone();
        cleared.set_attestation_tag(Vec::new());
        if !cleared.attestation_tag().is_empty() {
            return Err(TransportError::codec(WitnessTagError::InnerAttestationTag));
        }
        Ok((T::TAG, encoder.encode(&cleared)?))
    }

    fn decode_wire(
        tag: WitnessResponseTag,
        decoder: CanonicalBodyDecoder<'_>,
    ) -> Result<Self, TransportError> {
        if tag != T::TAG {
            return Err(TransportError::codec(WitnessTagError::Mismatch {
                expected: T::TAG.value(),
                actual: tag.value(),
            }));
        }
        decoder.decode()
    }
}

/// Applied and rejected answers with different postcard schemas.
///
/// The outer V1 tag, not a postcard discriminant, selects [`Self::Applied`] or
/// [`Self::Rejected`]. `A` and `R` each implement [`WitnessSchema`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DualWitnessAnswer<A, R> {
    /// Body encoded under `A::TAG`.
    Applied(A),
    /// Body encoded under `R::TAG`.
    Rejected(R),
}

impl<A: WitnessSchema, R: WitnessSchema> sealed::Sealed for DualWitnessAnswer<A, R> {}

impl<A: WitnessSchema, R: WitnessSchema> WitnessDomainWire for DualWitnessAnswer<A, R> {
    fn inputs_canonical_hash(&self) -> B256 {
        match self {
            Self::Applied(value) => value.inputs_canonical_hash(),
            Self::Rejected(value) => value.inputs_canonical_hash(),
        }
    }

    fn attestation_tag(&self) -> &[u8] {
        match self {
            Self::Applied(value) => value.attestation_tag(),
            Self::Rejected(value) => value.attestation_tag(),
        }
    }

    fn set_attestation_tag(&mut self, tag: Vec<u8>) {
        match self {
            Self::Applied(value) => value.set_attestation_tag(tag),
            Self::Rejected(value) => value.set_attestation_tag(tag),
        }
    }

    fn encode_wire(
        &self,
        encoder: CanonicalBodyEncoder,
    ) -> Result<(WitnessResponseTag, Vec<u8>), TransportError> {
        match self {
            Self::Applied(value) => value.encode_wire(encoder),
            Self::Rejected(value) => value.encode_wire(encoder),
        }
    }

    fn decode_wire(
        tag: WitnessResponseTag,
        decoder: CanonicalBodyDecoder<'_>,
    ) -> Result<Self, TransportError> {
        if tag == A::TAG {
            return Ok(Self::Applied(A::decode_wire(tag, decoder)?));
        }
        if tag == R::TAG {
            return Ok(Self::Rejected(R::decode_wire(tag, decoder)?));
        }
        Err(TransportError::codec(WitnessTagError::Mismatch {
            expected: A::TAG.value(),
            actual: tag.value(),
        }))
    }
}

/// Run a witness-recorded call.
///
/// The payload is `tag ||` canonical postcard of the schema selected by that
/// tag, with an empty inner `attestation_tag`. The entry stores
/// `inputs_canonical_hash` and the signature beside that payload. Replay
/// selects the schema from the tag, then checks the canonical body, the empty
/// inner tag, and the inner inputs hash before `authenticate`.
pub fn consensus_witness_call<T: WitnessDomainWire>(
    meta: CallMeta,
    execute: impl FnOnce(&InProcessTeeContext) -> Result<T, TransportError>,
    authenticate: impl Fn(&[u8; 32], &T) -> Result<(), TransportError>,
) -> Result<T, TransportError> {
    tee_consensus_typed_call(meta, execute, encode_domain_wire, move |key, wire| {
        decode_domain_wire(key, wire, &authenticate)
    })
}

fn encode_domain_wire<T: WitnessDomainWire>(
    value: &T,
) -> Result<TypedWitnessPayload, TransportError> {
    let inputs_canonical_hash = value.inputs_canonical_hash();
    let attestation_tag = value.attestation_tag().to_vec();
    let (tag, body) = value.encode_wire(CanonicalBodyEncoder(()))?;
    let checked = T::decode_wire(tag, CanonicalBodyDecoder(&body))?;
    if !checked.attestation_tag().is_empty() {
        return Err(TransportError::codec(WitnessTagError::InnerAttestationTag));
    }
    if checked.inputs_canonical_hash() != inputs_canonical_hash {
        return Err(TransportError::codec(WitnessTagError::InputsHashMismatch));
    }
    Ok(TypedWitnessPayload {
        payload: prefix_tag(tag, body),
        inputs_canonical_hash,
        attestation_tag,
    })
}

fn decode_domain_wire<T: WitnessDomainWire>(
    key: &[u8; 32],
    wire: outbe_tee::witness::TypedWitnessPayloadRef<'_>,
    authenticate: &impl Fn(&[u8; 32], &T) -> Result<(), TransportError>,
) -> Result<T, TransportError> {
    let (tag, body) = split_recordable_body(wire.payload)?;
    let mut decoded = T::decode_wire(tag, CanonicalBodyDecoder(body))?;
    if !decoded.attestation_tag().is_empty() {
        return Err(TransportError::codec(WitnessTagError::InnerAttestationTag));
    }
    if decoded.inputs_canonical_hash() != wire.inputs_canonical_hash {
        return Err(TransportError::codec(WitnessTagError::InputsHashMismatch));
    }
    decoded.set_attestation_tag(wire.attestation_tag.to_vec());
    authenticate(key, &decoded)?;
    Ok(decoded)
}

/// Run a non-consensus typed call. Returns `None` only when no runtime is installed.
pub fn session_witness_call<T>(
    meta: CallMeta,
    execute: impl FnOnce(&InProcessTeeContext) -> Result<T, TransportError>,
) -> Option<Result<T, TransportError>> {
    tee_session_typed_call(meta, execute)
}

#[cfg(any(test, feature = "test-utils"))]
thread_local! {
    static PAUSE_BEFORE_ACCOUNT_KEY_EXPORT: std::cell::Cell<Option<fn()>> =
        const { std::cell::Cell::new(None) };
}

/// Restores the previous account-query export pause on drop.
#[cfg(any(test, feature = "test-utils"))]
#[doc(hidden)]
pub struct AccountQueryPauseGuard {
    previous: Option<fn()>,
    _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(any(test, feature = "test-utils"))]
impl Drop for AccountQueryPauseGuard {
    fn drop(&mut self) {
        PAUSE_BEFORE_ACCOUNT_KEY_EXPORT.with(|slot| slot.set(self.previous.take()));
    }
}

/// Hold this thread inside `try_derive_sealed_account_keys`, before key export.
#[cfg(any(test, feature = "test-utils"))]
#[doc(hidden)]
pub fn pause_account_query_for_test(pause: fn()) -> AccountQueryPauseGuard {
    let previous = PAUSE_BEFORE_ACCOUNT_KEY_EXPORT.with(|slot| slot.replace(Some(pause)));
    AccountQueryPauseGuard {
        previous,
        _not_send: std::marker::PhantomData,
    }
}

#[cfg(any(test, feature = "test-utils"))]
fn pause_before_account_key_export() {
    if let Some(pause) = PAUSE_BEFORE_ACCOUNT_KEY_EXPORT.with(|slot| slot.take()) {
        pause();
    }
}

/// Seal one ledger's account keys for an owner-signed account query.
///
/// Returns `None` only when no resident runtime is installed.
pub fn try_derive_sealed_account_keys(
    ledger: Ledger,
    account: Address,
    requester_ephemeral: [u8; 32],
    owner_sig: [u8; 65],
) -> Option<Result<outbe_tee::resident::SealedAccountKeysExport, TransportError>> {
    let domain = domain_for(ledger);
    session_witness_call(
        CallMeta::session_only("derive_account_keys"),
        move |context| {
            let state_key = derive_resident_state_key(context, domain, 0)?;
            let keys = domain
                .account_keys
                .derive_account_keys(&state_key, account)
                .map_err(TransportError::enclave_error)?;
            #[cfg(any(test, feature = "test-utils"))]
            pause_before_account_key_export();
            domain
                .export_sealed_account_keys(&keys, &owner_sig, &requester_ephemeral)
                .map_err(TransportError::enclave_error)
        },
    )
}

/// Derive `domain`'s state key from the resident offer material bound to `context`.
pub fn derive_resident_state_key(
    context: &InProcessTeeContext,
    domain: &Domain,
    epoch: u64,
) -> Result<StateKeyHandle, TransportError> {
    context
        .state_key_handle(domain.state_info(), epoch)
        .map_err(|err| outbe_tee::InProcessDispatchError::Resident(err).into())
}

/// Seal a signed query's account keys using the resident context already captured by the caller.
pub fn seal_resident_account_keys<E, T>(
    context: &InProcessTeeContext,
    (domain, epoch): (&Domain, u64),
    (account, signature, requester): (Address, &[u8], &[u8; 32]),
    invalid_signature: impl FnOnce() -> E,
    into_response: impl FnOnce(Address, outbe_tee::resident::SealedAccountKeysExport) -> T,
) -> Result<T, TransportError>
where
    E: std::error::Error + Send + Sync + 'static,
{
    let signature: &[u8; 65] = signature
        .try_into()
        .map_err(|_| TransportError::enclave_error(invalid_signature()))?;
    let state_key = derive_resident_state_key(context, domain, epoch)?;
    let keys = domain
        .account_keys
        .derive_account_keys(&state_key, account)
        .map_err(TransportError::enclave_error)?;
    let sealed = domain
        .export_sealed_account_keys(&keys, signature, requester)
        .map_err(TransportError::enclave_error)?;
    Ok(into_response(account, sealed))
}

fn prefix_tag(tag: WitnessResponseTag, body: Vec<u8>) -> Vec<u8> {
    let mut payload = Vec::with_capacity(1 + body.len());
    payload.push(tag.value());
    payload.extend(body);
    payload
}

fn split_recordable_body(bytes: &[u8]) -> Result<(WitnessResponseTag, &[u8]), TransportError> {
    let (tag_byte, body) = bytes
        .split_first()
        .ok_or_else(|| TransportError::codec(WitnessTagError::MissingTag))?;
    let tag = WitnessResponseTag::try_from(*tag_byte).map_err(TransportError::codec)?;
    Ok((tag, body))
}

fn strip_tag(expected: WitnessResponseTag, bytes: &[u8]) -> Result<&[u8], WitnessTagError> {
    let (found, body) = bytes.split_first().ok_or(WitnessTagError::MissingTag)?;
    if NON_RECORDABLE_V1_TAGS.contains(found) {
        return Err(WitnessTagError::NonRecordable { tag: *found });
    }
    if *found != expected.value() {
        return Err(WitnessTagError::Mismatch {
            expected: expected.value(),
            actual: *found,
        });
    }
    Ok(body)
}

#[cfg(test)]
#[path = "witness_envelope_tests.rs"]
mod tests;
