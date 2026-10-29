//! Neutral wire-protocol types for the node <-> enclave channel.
//!
//! These types are the message contract shared by the host (`outbe-tee`) and
//! the enclave (`outbe-tee-enclave`). They carry **no secret material** and no
//! cryptographic logic. They define only the shape of requests and responses.
//!
//! Transport (later slice): length-prefixed framing over UDS, wrapped in a
//! Noise-IK transport (payload layer). Production first exposes only an
//! initialization challenge. A node-signed manifest installs one persistent
//! `NodeHost` initiator. The enclave accepts every later command, including quote
//! generation, only after Noise message 1 authenticates that initiator.
//! `GetQuote` exists only for the separate development transport.
//!
//! Opaque byte fields (`Vec<u8>`) intentionally hide DKG wire internals. The
//! host parses only the public envelope. It forwards the encrypted
//! secret-bearing parts to the enclave without decrypting them.

use alloy_primitives::{Address, B256, U256};

pub use outbe_primitives::time::WorldwideDay;

/// Canonical DirectDev onboarding context used by the typed enclave request.
pub type GramineDirectDevOnboardingContextV1 = crate::dcap_protocol::DcapOnboardingContextV1;

mod enclave_request;
mod enclave_response;

pub use enclave_request::EnclaveRequest;
pub use enclave_response::{
    derive_account_keys_message, derive_gratis_keys_message, eip191_hash,
    fidelity_cohort_attestation_preimage, fidelity_cohort_canonical_hash,
    fidelity_query_attestation_preimage, fidelity_query_auth_message,
    fidelity_query_canonical_hash, fidelity_snapshot_attestation_preimage,
    fidelity_snapshot_canonical_hash, gratis_op_attestation_preimage, gratis_op_canonical_hash,
    inputs_canonical_hash, promis_op_attestation_preimage, promis_op_canonical_hash,
    tribute_offer_attestation_preimage, EnclaveHealthStatusV1, EnclaveHeapUsageV1,
    EnclaveRequestClassCountsV1, EnclaveRequestCountsV1, EnclaveResponse,
};

/// How a request participates in the block's enclave witness tape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsensusWitnessKind {
    Recorded,
    StreamFrame,
    NonConsensus,
}

/// Hard cap for the deterministic registry onboarding artifact. The current
/// X25519/nonce/AEAD envelope is substantially smaller. This cap prevents a
/// malformed enclave response from creating an unbounded consensus log.
pub const MAX_ONBOARDING_ARTIFACT_BYTES: usize = 512;

/// Minimum canonical framing: the committed 32-byte offer public key plus the
/// ephemeral public key, nonce and authenticated ciphertext framing.
pub const MIN_ONBOARDING_ARTIFACT_BYTES: usize = 60;

/// A single offer handed to the enclave.
///
/// Fields mirror the part of `ITributeFactory.offerTribute` the enclave needs,
/// plus the sender and the node-resolved public Oracle inputs:
///   - `cipherText`, `nonce`, `ephemeralPubkey`, `worldwideDay`,
///     `tributeCurrency`, `referenceCurrency` (ABI).
///   - `owner`: the L1 `msg.sender`. The enclave binds it into the result and
///     into the `token_id` (computed in-enclave, see `TributeOfferResult`).
///   - `issuance_wwd_vwap_minor`, `reference_wwd_vwap_minor`, and
///     `reference_scurve_minor`: the node resolves them from committed Oracle
///     state. They are not ABI fields.
///
/// Before the enclave call, the host:
///   - requires a registered L2 operator.
///   - validates the root signature.
///   - resolves the exact circuit version.
///   - checks proof framing.
///
/// After decryption, the host compares the enclave's expected hashes and verifies
/// with the selected key. The host does not forward raw proof/signature bytes or
/// circuit versions. The L2 chain id binds the decrypted claim.
///
/// Every field here is public and host-supplied, so the enclave never echoes any
/// of them back. [`TributeOfferResult`] carries only what the enclave itself
/// computed from the decrypted payload.
///
/// Price integrity: the enclave applies the rate but does not verify it against
/// chain state. Deterministic re-execution enforces integrity (a forged rate
/// yields a state-root mismatch). See plan section "Oracle Price Determinism".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EncryptedTributeOffer {
    /// L1 `msg.sender` that owns the resulting Tribute (public, on-chain).
    pub owner: Address,
    /// ABI `cipherText`: AEAD ciphertext of the offer payload.
    pub cipher_text: Vec<u8>,
    /// ABI `nonce`: 12-byte ChaCha20Poly1305 nonce.
    pub nonce: Vec<u8>,
    /// ABI `ephemeralPubkey` (uint256): client ephemeral X25519 public key for
    /// ECDHE, big-endian.
    pub ephemeral_pubkey: U256,
    /// ABI `worldwideDay`: UTC+14 day key (`YYYYMMDD`). The node settles calendar
    /// validity and OFFERING status before the call. The enclave binds the value
    /// into `token_id` without re-deriving either. The host recomputes the same
    /// `(owner, day)` identity from its own input and rejects a mismatch. Every
    /// validator re-executes the call. Thus the chain anchors this field, not a
    /// second in-enclave calendar.
    pub worldwide_day: WorldwideDay,
    /// ABI `tributeCurrency`: ISO 4217 code the tribute amount is denominated in.
    pub tribute_currency: u16,
    /// ABI `referenceCurrency`. A separate axis from `tribute_currency`. It
    /// denominates the rights' entry and call prices, not pricing.
    pub reference_currency: u16,
    /// Exact WorldwideDay VWAP for `COEN/<tribute_currency>`, scale 1e6.
    pub issuance_wwd_vwap_minor: U256,
    /// Exact WorldwideDay VWAP for `COEN/<reference_currency>`, scale 1e6.
    pub reference_wwd_vwap_minor: U256,
    /// Continuous reference-currency S-curve value, scale 1e6. Zero means no
    /// active curve and is valid.
    pub reference_scurve_minor: U256,
    /// Public ZK claim context supplied for every admitted offer. The owner is
    /// the first public input in `zkProof`. The host chain id comes from the
    /// local execution context, and the L2 chain id from the verified network.
    #[serde(default)]
    pub zk_context: Option<TributeZkContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TributeZkContext {
    pub derived_owner: B256,
    pub chain_id: u64,
    pub l2_chain_id: u64,
}

/// Shared four-word public claim for Demo Tribute and Niflheim Tribute.
/// The first word is `derived_owner` in Demo's ABI and `owner` in Niflheim's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TributePublicInputs {
    pub derived_owner: B256,
    pub nft_hash: B256,
    pub binding_hash: B256,
    pub merkle_root: B256,
}

impl TributePublicInputs {
    /// Map ABI-ordered words after the caller validates canonical field encoding.
    pub fn from_raw_parts(
        [derived_owner, nft_hash, binding_hash, merkle_root]: [[u8; 32]; 4],
    ) -> Self {
        Self {
            derived_owner: B256::from(derived_owner),
            nft_hash: B256::from(nft_hash),
            binding_hash: B256::from(binding_hash),
            merkle_root: B256::from(merkle_root),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TributeZkExpectedHashes {
    pub nft_hash: B256,
    pub binding_hash: B256,
}

/// Status of a single offer after enclave processing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TributeOfferStatus {
    Created,
    Rejected { reason: String },
}

/// Public result for a single offer (Enclave Return Rule: no L2 draft owner, no
/// L2 pubkey, no raw proof witness).
///
/// `token_id` is computed **inside the enclave** via Poseidon over sensitive
/// decrypted data. The host never sees that data, so it cannot derive the value.
/// `owner` is the L1 `msg.sender`, bound by the enclave. The remaining
/// fields are the economics derived from the decrypted payload.
///
/// This carries **only what the enclave computed**. The enclave does not echo back
/// the values that the host supplied in [`EncryptedTributeOffer`] (day, currencies,
/// price). The host already holds them, so an echo would be one more thing to
/// keep in agreement for no gain. `owner` is the deliberate
/// exception. The enclave folds it into `token_id`, so a comparison against
/// `msg.sender` checks the enclave's computation rather than repeating an input.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TributeOfferResult {
    /// Poseidon(token_id preimage), computed in-enclave from sensitive data.
    pub token_id: B256,
    /// L1 `msg.sender` (public, on-chain).
    pub owner: Address,
    pub issuance_amount_minor: U256,
    pub nominal_amount_minor: U256,
    /// `max(reference_wwd_vwap_minor, reference_scurve_minor)`, computed inside
    /// the enclave and checked by the host against the public request inputs.
    pub effective_reference_price_minor: U256,
    /// SU hashes (hex). The host marks them used (replay prevention). Public
    /// on-chain as used-markers. The privacy-preserving markers-only form (rather
    /// than raw hashes) is a later slice (see `process.rs`).
    pub su_hashes: Vec<String>,
    /// WAA wallet addresses. The host routes agent rewards. Public on-chain.
    pub wallet_addresses: Vec<String>,
    /// SRA addresses. The host routes agent rewards. Public on-chain.
    pub sra_addresses: Vec<String>,
    /// Expected public hashes recomputed over the decrypted TributeDraft.
    /// Present only when the matching request carried [`TributeZkContext`].
    #[serde(default)]
    pub zk_expected_hashes: Option<TributeZkExpectedHashes>,
    pub status: TributeOfferStatus,
}

/// Requests sent from the node to the enclave.
///
/// DKG secret-seam variants carry opaque bytes: the host never sees plaintext
/// shares.
/// One DKG participant's ceremony-scoped identity. `enc_sig` authenticates:
/// - the full network binding
/// - the ceremony id and round
/// - the exact participant-set hash
/// - the X25519 share-recipient key
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ParticipantAnnounce {
    /// Encoded TEE-BLS public key (the participant's DKG identity).
    pub bls_pub: Vec<u8>,
    /// Announced X25519 share-encryption public key.
    pub enc_pub: [u8; 32],
    pub ceremony_id: B256,
    pub round: u64,
    pub participant_set_hash: B256,
    /// TEE-BLS signature over the complete ceremony-scoped announcement.
    pub enc_sig: Vec<u8>,
}

/// A Gratis write operation the enclave applies over encrypted per-account state.
///
/// The op selects the account's liquid and pledged balance transitions.
/// The runtime tracks the aggregate pledged backing and authorizes collateral operations.
/// Mint, Burn and Pledge require the owner's modify authorization.
/// Their discriminants stay fixed because the modify MAC binds the operation tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GratisOp {
    Mint,
    Burn,
    /// Move the authorized amount from the liquid to the pledged balance.
    Pledge,
    /// Move collateral from the pledged back to the liquid balance.
    ReleasePledged,
    /// Burn collateral from the pledged balance.
    BurnPledged,
}

/// Proof that the caller holds the account's modify key, without revealing it.
///
/// `mac = HMAC-SHA256(modify_key, "outbe/gratis/modify/v1" || account || op_tag ||
/// amount || op_nonce || chain_id)`. The enclave recomputes it and re-derives
/// `modify_key` from the resident state key + account. `op_nonce` is the
/// account's monotonic on-chain replay counter, so an attacker cannot replay a
/// captured tuple.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModifyAuth {
    pub mac: [u8; 32],
    pub op_nonce: u64,
}

/// Stateless transition over an account's liquid and pledged balance blobs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GratisOpRequest {
    pub op: GratisOp,
    pub chain_id: B256,
    pub account: Address,
    pub amount: U256,
    pub current_balance: Vec<u8>,
    pub current_pledged: Vec<u8>,
    pub modify_auth: ModifyAuth,
    pub fidelity: Option<FidelityOpSection>,
}

/// The Fidelity cohort mutation carried inside a Gratis op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FidelityCohortOp {
    /// Acquisition: push a new active cohort of the Gratis op's `amount`.
    In,
    /// Sale: consume active cohorts LIFO (proportional boundary split) for the
    /// Gratis op's `amount`.
    Out,
    /// Read-only league probe (no cohort mutation, no blob rewrite). The pledge
    /// eligibility gate uses it to learn the caller's league in the same trip.
    Probe,
}

/// Co-located Fidelity input riding in a [`GratisOpRequest`]. The host reads the
/// account's current cohort blob from committed storage and forwards it
/// verbatim. Account + amount are the Gratis op's own fields.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityOpSection {
    pub op: FidelityCohortOp,
    /// Block timestamp (seconds). It is the cohort `acquired_at`/`sold_at` stamp
    /// and the league evaluation time.
    pub timestamp: u64,
    /// Plaintext global `first_qualified_start` scalar (league ceiling anchor).
    /// `0` before any account qualified.
    pub first_qualified_start: u64,
    /// Current cohort-ledger blob (`version(8) || FID2 || binding(32) || ciphertext`); empty when the
    /// account has no cohort state yet.
    pub current_blob: Vec<u8>,
}

/// Plaintext receipt of a [`FidelityOpSection`], returned inside the
/// [`GratisOpResult`]. Cohort contents never appear here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityOpOutcome {
    /// New cohort-ledger blob (`version || FID2 || binding || ct`) to store verbatim; EMPTY for a
    /// `Probe` (nothing to write).
    pub new_blob: Vec<u8>,
    /// `Some(ts)` when this op set the account's `qualified_start` (first
    /// acquisition). The host then updates the global plaintext
    /// `first_qualified_start` if it is still unset.
    pub qualified_start_initialized: Option<u64>,
    /// The account's league at the section timestamp, evaluated post-op.
    pub league: u16,
}

/// Inputs for a STANDALONE `ApplyFidelityCohortOp`: a cohort mutation applied
/// on its own enclave round-trip. Callers use it where there is no co-located
/// Gratis op to fold into, i.e. the fidelity crate's `cohort_in`/`cohort_out`
/// before the Phase-3 round-trip fold. The section carries the
/// op/timestamp/anchor/blob. `account` + `amount` are the mutation's subject.
/// Consensus path (called from precompile-driven factory flows, re-executed by
/// every validator).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityCohortRequest {
    pub chain_id: B256,
    pub account: Address,
    pub amount: U256,
    pub section: FidelityOpSection,
}

/// Public result of an `ApplyFidelityCohortOp`: the plaintext outcome plus the
/// determinism/attestation material. Cohort contents never appear here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityCohortResult {
    pub outcome: FidelityOpOutcome,
    /// Diagnostic hash of the canonical request inputs. The host recomputes it to
    /// detect enclave non-determinism, then discards it.
    pub inputs_canonical_hash: B256,
    /// Local-only attestation tag over `(inputs_canonical_hash || result)`. The
    /// host verifies it against the pinned enclave attestation key, then discards
    /// it.
    pub attestation_tag: Vec<u8>,
}

/// Outcome of a single Gratis op.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GratisOpStatus {
    Applied,
    Rejected { reason: String },
}

/// Public result of an `ApplyGratisOp`: the new ciphertext blobs to store verbatim
/// plus the plaintext receipt the host needs (aggregate deltas, event amount).
/// Per-account plaintext balances never appear here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GratisOpResult {
    pub status: GratisOpStatus,
    pub new_balance: Vec<u8>,
    pub new_pledged: Vec<u8>,
    pub event_amount: U256,
    pub next_op_nonce: u64,
    pub fidelity: Option<FidelityOpOutcome>,
    pub inputs_canonical_hash: B256,
    pub attestation_tag: Vec<u8>,
}

/// The confidential ledger that a key-derivation / op request targets. It selects
/// the enclave key domain (state/view/modify HKDF labels). Thus Gratis, Promis,
/// Fidelity, Tribute, and Nod derive cryptographically independent keys from the
/// same resident group signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Ledger {
    Gratis,
    Promis,
    /// The encrypted per-account cohort ledger (Fidelity). View keys decrypt the
    /// cohort blob client-side. There is no user-held modify capability (the chain
    /// initiates cohort ops inside Gratis ops).
    Fidelity,
    /// The encrypted Tribute entity domain. View keys decrypt `EncryptedTributeV2`
    /// amounts client-side.
    Tribute,
    /// The encrypted NOD entity domain. View keys decrypt `EncryptedNodV1`
    /// amounts client-side.
    Nod,
}

/// A Promis write operation the enclave applies over the encrypted per-account
/// balance. Promis is a mint/burn-only confidential ledger (no pledge/credis
/// machinery), so its op set is a strict subset of [`GratisOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PromisOp {
    /// Mint `amount` to `account` (credit balance, `total_supply += amount`).
    Mint,
    /// Burn `amount` from `account` (debit balance, `total_supply -= amount`).
    Burn,
}

/// Inputs for a single `ApplyPromisOp`. The host reads the current balance
/// ciphertext (`version(8 BE) || PRO2 || binding(32) || ct`, empty for a fresh account)
/// from committed storage and forwards it verbatim. The enclave then:
/// 1. decrypts the ciphertext.
/// 2. enforces the balance invariant + modify-key authorization.
/// 3. re-encrypts deterministically.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PromisOpRequest {
    pub op: PromisOp,
    pub chain_id: B256,
    pub account: Address,
    pub amount: U256,
    /// Current balance blob (`version(8 BE) || PRO2 || binding(32) || ciphertext`).
    /// Empty when the account has no state yet.
    pub current_balance: Vec<u8>,
    /// Modify-key authorization (required for both Mint and Burn).
    pub modify_auth: ModifyAuth,
}

/// Outcome of a single Promis op.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PromisOpStatus {
    Applied,
    Rejected { reason: String },
}

/// Public result of an `ApplyPromisOp`: the new balance ciphertext to store
/// verbatim plus the plaintext receipt (event amount, next op-nonce). The
/// per-account plaintext balance never appears here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PromisOpResult {
    pub status: PromisOpStatus,
    /// New balance blob (`version || PRO2 || binding || ct`) to store verbatim.
    pub new_balance: Vec<u8>,
    /// Amount for the emitted event (mint/burn magnitude).
    pub event_amount: U256,
    /// The account's next modify-auth nonce (for the host to persist).
    pub next_op_nonce: u64,
    /// Diagnostic hash of the canonical request inputs. The host recomputes it to
    /// detect enclave non-determinism, then discards it.
    pub inputs_canonical_hash: B256,
    /// Local-only attestation tag over `(inputs_canonical_hash || result)`. The host
    /// verifies it against the pinned enclave attestation key, then discards it.
    pub attestation_tag: Vec<u8>,
}

/// One owner's encrypted cohort blob in a [`FidelitySnapshotRequest`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelitySnapshotEntry {
    pub owner: Address,
    /// Current cohort-ledger blob (`version(8) || FID2 || binding(32) || ct`); empty for no state.
    pub cohort_blob: Vec<u8>,
}

/// Inputs for a `SnapshotFidelityLeagues` batch: metadosis's once-per-WWD league
/// snapshot over the day's tribute owners. The host reads each owner's cohort
/// blob from committed storage and forwards it verbatim. The enclave decrypts and
/// returns one plaintext league word per owner. Consensus path (called from the
/// Metadosis preparation step in begin-block, re-executed by every validator).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelitySnapshotRequest {
    /// League evaluation time (the WWD's intent-bound snapshot timestamp).
    pub timestamp: u64,
    /// Plaintext global `first_qualified_start` scalar. `0` if unset.
    pub first_qualified_start: u64,
    pub entries: Vec<FidelitySnapshotEntry>,
}

/// One owner's plaintext league and RCFI in a snapshot result, in request order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityLeagueEntry {
    pub owner: Address,
    pub league: u16,
    #[serde(default)]
    pub rcfi: U256,
}

/// Inputs for a `QueryFidelityIndex`: an owner-authorized read of one account's
/// RCFI/league over its encrypted cohorts. Consensus when executed in a transaction;
/// also served via `eth_call`. `owner_sig` is the 65-byte EIP-191
/// `personal_sign` signature by
/// `account` over [`fidelity_query_auth_message`]. The enclave recovers it and
/// rejects the request unless all of these are true:
/// - the signer equals `account`.
/// - the message chain id equals the enclave's resident chain id.
/// - `expiry >= block_timestamp`.
///
/// Scope of the guarantees: the signature is never key material and can never
/// be forged. The enclave DOES enforce chain binding. It hashes the message under
/// its own resident chain id and rejects a mismatched `chain_id`. Thus a signature
/// captured on another chain (same reused EOA) cannot authorize a read here.
/// The `expiry` bound is only advisory against a COMPROMISED host. The enclave
/// has no trusted clock on the `eth_call` path and checks `expiry` against the
/// host-supplied `block_timestamp`. Thus a malicious host can pass
/// `block_timestamp = 0` and reuse a stale genuine signature. The worst case is
/// re-reading the derived index/league that the owner already chose to expose by
/// signing. The worst case never includes the raw cohort ledger.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityQueryRequest {
    /// The chain the authorization is for. The enclave rejects unless it equals
    /// its own resident chain id. The enclave does NOT trust this value for key
    /// derivation. Key derivation uses the resident id.
    pub chain_id: B256,
    pub account: Address,
    /// Current cohort-ledger blob (`version(8) || FID2 || binding(32) || ct`); empty for no state.
    pub cohort_blob: Vec<u8>,
    /// Timestamp to evaluate RCFI/league at (any time, because the curve is pure).
    pub query_timestamp: u64,
    /// Current block timestamp, for the `expiry` freshness check (advisory
    /// against a compromised host, see the type doc).
    pub block_timestamp: u64,
    /// Plaintext global `first_qualified_start` scalar. `0` if unset.
    pub first_qualified_start: u64,
    /// Authorization deadline (seconds). The signature is valid until then.
    pub expiry: u64,
    /// 65-byte `r||s||v` signature. It is a Vec because serde does not derive for
    /// `[u8; 65]`. The enclave validates the length.
    pub owner_sig: Vec<u8>,
}

/// Plaintext result of a `QueryFidelityIndex` (10^18-scaled fixed point, same
/// as the historical `IFidelity` values).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityQueryResult {
    pub rcfi: U256,
    pub efficiency: U256,
    pub league: u16,
    /// Diagnostic hash of the canonical request inputs. The host recomputes it to
    /// detect enclave non-determinism, then discards it.
    pub inputs_canonical_hash: B256,
    /// Local-only attestation tag over `(inputs_canonical_hash || result)`. The host
    /// verifies it against the pinned enclave attestation key, then discards it.
    pub attestation_tag: Vec<u8>,
}
