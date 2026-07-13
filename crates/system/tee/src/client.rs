//! Blocking node-side client for the enclave channel (Noise-IK over framed UDS).
//!
//! Flow: connect -> `GetQuote` (cleartext, unauthenticated)
//! -> verify quote against policy + REPORT_DATA key binding -> pin the enclave
//! Noise static key -> Noise-IK handshake -> encrypted request/response.
//!
//! The client is fully synchronous: it is meant to be driven straight from the
//! `offerTributeBatch` precompile path with a blocking UDS round-trip — no
//! async, no `spawn`, nothing that would capture a `StorageHandle`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::Path;

use alloy_primitives::{keccak256, B256};

use crate::codec::{decode_response, encode_request, read_frame, write_frame};
use crate::errors::TransportError;
use crate::protocol::{EnclaveRequest, EnclaveResponse};
use crate::NOISE_PARAMS;

/// The byte carrier under the Noise-IK session: a local Unix domain socket
/// (native sidecar) or TCP (used when the enclave runs under Gramine, whose
/// pathname UDS are process-internal). Noise authenticates + encrypts every
/// byte regardless, so the carrier does not change the channel's security.
enum Transport {
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl Read for Transport {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Transport::Unix(s) => s.read(buf),
            Transport::Tcp(s) => s.read(buf),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Transport::Unix(s) => s.write(buf),
            Transport::Tcp(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Transport::Unix(s) => s.flush(),
            Transport::Tcp(s) => s.flush(),
        }
    }
}

/// Genesis-style attestation policy used to verify the enclave quote.
#[derive(Debug, Clone)]
pub struct QuotePolicy {
    pub allowed_mrenclave: Vec<B256>,
    pub allowed_mrsigner: Vec<B256>,
    pub min_isv_svn: u16,
    /// Dev/test escape hatch: accept any MRENCLAVE/MRSIGNER (still enforces the
    /// REPORT_DATA key binding). MUST be false in production.
    pub dev_accept_any_measurement: bool,
    /// Mode gate: when true, an **unattested** enclave (empty quote, e.g.
    /// gramine-direct / no SGX hardware) is accepted with only the REPORT_DATA key
    /// binding enforced. A **real** (non-empty) quote is ALWAYS strictly verified
    /// regardless of this flag — measurement allowlist + min SVN + DCAP signature.
    /// This lets one policy be strict under gramine-sgx yet still run on the
    /// gramine-direct dev box. Independent of `dev_accept_any_measurement`, which
    /// relaxes even a real quote (dev/test only).
    pub dev_fallback_if_unattested: bool,
}

impl QuotePolicy {
    pub fn new(
        allowed_mrenclave: Vec<B256>,
        allowed_mrsigner: Vec<B256>,
        min_isv_svn: u16,
    ) -> Self {
        Self {
            allowed_mrenclave,
            allowed_mrsigner,
            min_isv_svn,
            dev_accept_any_measurement: false,
            dev_fallback_if_unattested: false,
        }
    }

    /// Dev policy: accept any measurement, but still enforce the key binding.
    pub fn dev_accept_any() -> Self {
        Self {
            allowed_mrenclave: Vec::new(),
            allowed_mrsigner: Vec::new(),
            min_isv_svn: 0,
            dev_accept_any_measurement: true,
            dev_fallback_if_unattested: true,
        }
    }

    /// Strict policy built from the genesis `teePolicy` allowlist: a real
    /// SGX quote MUST satisfy the measurement allowlist + min SVN + DCAP signature;
    /// an unattested enclave (gramine-direct / no SGX) is still accepted on the dev
    /// box via `dev_fallback_if_unattested`, logged as not-confidential by the
    /// caller. Under gramine-sgx this is the real measurement gate.
    pub fn from_genesis_strict(
        allowed_mrenclave: Vec<B256>,
        allowed_mrsigner: Vec<B256>,
        min_isv_svn: u16,
    ) -> Self {
        Self {
            allowed_mrenclave,
            allowed_mrsigner,
            min_isv_svn,
            dev_accept_any_measurement: false,
            dev_fallback_if_unattested: true,
        }
    }
}

/// The attested enclave identity captured from the quote at connect time, used
/// to build this validator's [`crate::bootstrap::EnclaveRegistration`].
#[derive(Debug, Clone)]
struct QuoteIdentity {
    mrenclave: B256,
    mrsigner: B256,
    isv_svn: u16,
    recipient_x25519: [u8; 32],
    attestation_pub: [u8; 32],
    noise_static_pub: [u8; 32],
    /// The attestation environment the enclave self-reported (e.g.
    /// `none (gramine-direct / no SGX)`).
    attestation: String,
}

/// Blocking client for one enclave session.
pub struct EnclaveClient {
    stream: Transport,
    noise: snow::TransportState,
    identity: QuoteIdentity,
    /// The raw `EnclaveResponse::Quote` this session verified at connect. Retained
    /// so a key-handoff newcomer can forward its own attested quote to a server
    /// (which re-verifies it via [`verify_peer_quote`] before sealing).
    raw_quote: EnclaveResponse,
}

/// Bound on a single blocking enclave read/write: a wedged enclave that accepts
/// the connection but never responds surfaces as a timeout error instead of hanging
/// the caller (e.g. node startup) forever. Generous — every real enclave op (quote,
/// Noise handshake, seal, offer-batch decrypt) completes well within this.
const ENCLAVE_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

impl EnclaveClient {
    /// Connect to the enclave over a Unix domain socket (native sidecar), then
    /// fetch+verify the quote, pin the enclave Noise static key, and complete the
    /// Noise-IK handshake.
    pub fn connect(path: &Path, policy: &QuotePolicy) -> Result<Self, TransportError> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(ENCLAVE_IO_TIMEOUT))?;
        stream.set_write_timeout(Some(ENCLAVE_IO_TIMEOUT))?;
        Self::from_transport(Transport::Unix(stream), policy)
    }

    /// Connect to the enclave over TCP (`host:port`) — used when the enclave runs
    /// under Gramine, whose pathname UDS cannot be reached from a host process.
    pub fn connect_tcp(addr: &str, policy: &QuotePolicy) -> Result<Self, TransportError> {
        let stream = TcpStream::connect(addr)?;
        let _ = stream.set_nodelay(true);
        stream.set_read_timeout(Some(ENCLAVE_IO_TIMEOUT))?;
        stream.set_write_timeout(Some(ENCLAVE_IO_TIMEOUT))?;
        Self::from_transport(Transport::Tcp(stream), policy)
    }

    /// True only if the connected enclave is REMOTE-attested — it produced a real
    /// DCAP/EPID quote. Measurements alone are NOT sufficient: under gramine-sgx
    /// with `sgx.remote_attestation = "none"` the enclave reports REAL measurements
    /// (read from the local SGX report) but produces no quote, so it is confidential
    /// and measured yet unattested. False under gramine-direct / bare too. Gate
    /// quote-dependent trust on this, not on non-zero measurements.
    pub fn is_hardware_attested(&self) -> bool {
        let a = &self.identity.attestation;
        a.starts_with("dcap") || a.starts_with("epid")
    }

    /// The connected enclave's measurements `(mrenclave, mrsigner, isv_svn)`.
    pub fn measurements(&self) -> (B256, B256, u16) {
        (
            self.identity.mrenclave,
            self.identity.mrsigner,
            self.identity.isv_svn,
        )
    }

    /// The exact attestation environment the enclave self-reported (e.g.
    /// `dcap (gramine-sgx)` or `none (gramine-direct / no SGX)`).
    pub fn attestation_label(&self) -> &str {
        &self.identity.attestation
    }

    /// The enclave's Ed25519 attestation public key, pinned from this session's
    /// quote (its `report_data` binding was verified at connect). Used to verify
    /// per-offer attestation tags (`verify_tribute_offer_attestation`) — a local
    /// verify-then-discard check that proves a batch's results were produced
    /// inside this attested enclave.
    pub fn attestation_pub(&self) -> [u8; 32] {
        self.identity.attestation_pub
    }

    /// Connect using an endpoint string: `host:port` → TCP, otherwise a UDS path.
    pub fn connect_endpoint(endpoint: &str, policy: &QuotePolicy) -> Result<Self, TransportError> {
        if endpoint.contains(':') {
            Self::connect_tcp(endpoint, policy)
        } else {
            Self::connect(Path::new(endpoint), policy)
        }
    }

    /// Run the GetQuote + verification + Noise-IK handshake over an established
    /// transport.
    fn from_transport(mut stream: Transport, policy: &QuotePolicy) -> Result<Self, TransportError> {
        // 1. GetQuote (cleartext, pre-handshake) with a fresh nonce.
        let nonce: [u8; 32] = rand::random();
        write_frame(
            &mut stream,
            &encode_request(&EnclaveRequest::GetQuote { nonce })?,
        )?;
        let quote = decode_response(&read_frame(&mut stream)?)?;
        let enclave_static = verify_quote(&quote, policy)?;
        let identity = quote_identity(&quote)?;

        // 2. Noise-IK handshake (initiator). The host static key is ephemeral
        //    per connection; the enclave static key is the attested, pinned one.
        let params = NOISE_PARAMS
            .parse()
            .map_err(|e| TransportError::Noise(format!("{e:?}")))?;
        let builder = snow::Builder::new(params);
        let host_keys = builder
            .generate_keypair()
            .map_err(|e| TransportError::Noise(e.to_string()))?;
        let mut handshake = builder
            .local_private_key(&host_keys.private)
            .remote_public_key(&enclave_static)
            .build_initiator()
            .map_err(|e| TransportError::Handshake(e.to_string()))?;

        let mut buf = [0u8; 1024];
        let n = handshake
            .write_message(&[], &mut buf)
            .map_err(|e| TransportError::Handshake(e.to_string()))?;
        write_frame(&mut stream, &buf[..n])?;

        let msg2 = read_frame(&mut stream)?;
        handshake
            .read_message(&msg2, &mut buf)
            .map_err(|e| TransportError::Handshake(e.to_string()))?;

        let noise = handshake
            .into_transport_mode()
            .map_err(|e| TransportError::Handshake(e.to_string()))?;
        Ok(Self {
            stream,
            noise,
            identity,
            raw_quote: quote,
        })
    }

    /// This session's attested quote (verified at connect). A key-handoff newcomer
    /// forwards it to a server so the server can authenticate it independently.
    pub fn quote(&self) -> &EnclaveResponse {
        &self.raw_quote
    }

    /// Send one request, read one response, encrypted under the session.
    pub fn request(&mut self, req: &EnclaveRequest) -> Result<EnclaveResponse, TransportError> {
        let plain = encode_request(req)?;
        let mut ct = vec![0u8; plain.len() + 64];
        let n = self
            .noise
            .write_message(&plain, &mut ct)
            .map_err(|e| TransportError::Noise(e.to_string()))?;
        write_frame(&mut self.stream, &ct[..n])?;

        let resp_ct = read_frame(&mut self.stream)?;
        let mut pt = vec![0u8; resp_ct.len()];
        let n = self
            .noise
            .read_message(&resp_ct, &mut pt)
            .map_err(|e| TransportError::Noise(e.to_string()))?;
        let resp = decode_response(&pt[..n])?;
        if let EnclaveResponse::Error { message } = &resp {
            return Err(TransportError::EnclaveError(message.clone()));
        }
        Ok(resp)
    }

    /// This validator's TEE registration, built from the attested quote captured
    /// at connect (recipient X25519 / attestation / noise keys + measurements).
    /// `keys_hash` is derived by the bootstrap builder; `validator` is the L1
    /// validator address this enclave serves.
    pub fn enclave_registration(
        &self,
        validator: alloy_primitives::Address,
    ) -> crate::bootstrap::EnclaveRegistration {
        crate::bootstrap::EnclaveRegistration {
            validator,
            recipient_x25519: B256::from(self.identity.recipient_x25519),
            attestation_pub: B256::from(self.identity.attestation_pub),
            noise_static_pub: B256::from(self.identity.noise_static_pub),
            mrenclave: self.identity.mrenclave,
            mrsigner: self.identity.mrsigner,
            isv_svn: self.identity.isv_svn,
        }
    }
}

/// Extract the attested enclave identity from the quote response.
fn quote_identity(quote: &EnclaveResponse) -> Result<QuoteIdentity, TransportError> {
    let EnclaveResponse::Quote {
        mrenclave,
        mrsigner,
        isv_svn,
        recipient_x25519_pub,
        attestation_pub,
        noise_static_pub,
        attestation,
        ..
    } = quote
    else {
        return Err(TransportError::UnexpectedResponse);
    };
    Ok(QuoteIdentity {
        mrenclave: *mrenclave,
        mrsigner: *mrsigner,
        isv_svn: *isv_svn,
        recipient_x25519: *recipient_x25519_pub,
        attestation_pub: *attestation_pub,
        noise_static_pub: *noise_static_pub,
        attestation: attestation.clone(),
    })
}

/// The attested public keys carried in a verified quote — all three are bound
/// into REPORT_DATA, so a successful verification proves the enclave (not the
/// host) owns them. Returned by [`verify_peer_quote`].
#[derive(Clone, Copy, Debug)]
pub struct AttestedPeerKeys {
    pub recipient_x25519: [u8; 32],
    pub attestation_pub: [u8; 32],
    pub noise_static_pub: [u8; 32],
}

/// Verify a peer enclave's quote standalone and return its attested keys. Used at
/// connect time (the connect path takes `noise_static_pub` to pin) and to
/// authenticate a key-handoff newcomer whose quote arrives over P2P (the server
/// then seals to the attested `recipient_x25519`).
///
/// Chain: (1) the cleartext public keys must hash to `report_data` (key
/// binding); (2) if the enclave produced a real SGX quote, the cleartext
/// measurements + report_data must match what the hardware actually signed, and
/// a strict policy additionally requires DCAP signature verification; an empty
/// quote (gramine-direct/bare) is accepted only by a dev policy; (3) the
/// measurements must satisfy the policy allowlist + min SVN.
pub fn verify_peer_quote(
    quote: &EnclaveResponse,
    policy: &QuotePolicy,
) -> Result<AttestedPeerKeys, TransportError> {
    let EnclaveResponse::Quote {
        mrenclave,
        mrsigner,
        isv_svn,
        report_data,
        recipient_x25519_pub,
        attestation_pub,
        noise_static_pub,
        quote_body,
        attestation: _,
    } = quote
    else {
        return Err(TransportError::UnexpectedResponse);
    };

    // (1) REPORT_DATA binds the cleartext public keys to the attestation.
    let mut preimage = Vec::with_capacity(96);
    preimage.extend_from_slice(noise_static_pub);
    preimage.extend_from_slice(recipient_x25519_pub);
    preimage.extend_from_slice(attestation_pub);
    let binding = keccak256(&preimage);
    if binding != *report_data {
        return Err(TransportError::Attestation(
            "report_data key binding mismatch".to_string(),
        ));
    }

    // (2) Hardware quote vs unattested. A real (non-empty) quote is ALWAYS
    // strictly verified (its measurements come from what the hardware signed);
    // an empty quote (gramine-direct / no SGX) is accepted only by a dev or
    // unattested-fallback policy — never with the measurement gate (its
    // measurements are ZERO, not a real allowlist entry).
    if !quote_body.is_empty() {
        let m = crate::quote::parse_quote_measurements(quote_body)
            .map_err(|e| TransportError::Attestation(format!("quote parse: {e}")))?;
        if B256::from(m.mrenclave) != *mrenclave
            || B256::from(m.mrsigner) != *mrsigner
            || m.isv_svn != *isv_svn
        {
            return Err(TransportError::Attestation(
                "cleartext measurements do not match the quote".to_string(),
            ));
        }
        if m.report_data[..32] != binding.as_slice()[..] {
            return Err(TransportError::Attestation(
                "quote report_data does not match the key binding".to_string(),
            ));
        }
        // Strict path (real quote, not dev-accept-any): DCAP signature/TCB +
        // measurement allowlist + min SVN. `dev_fallback_if_unattested` does NOT
        // relax a real quote — only `dev_accept_any_measurement` does.
        if !policy.dev_accept_any_measurement {
            crate::quote::verify_dcap_signature(quote_body)
                .map_err(|e| TransportError::Attestation(format!("DCAP verify: {e}")))?;
            if !policy.allowed_mrsigner.contains(mrsigner) {
                return Err(TransportError::Attestation(format!(
                    "mrsigner {mrsigner} not in policy"
                )));
            }
            if !policy.allowed_mrenclave.contains(mrenclave) {
                return Err(TransportError::Attestation(format!(
                    "mrenclave {mrenclave} not in policy"
                )));
            }
            if *isv_svn < policy.min_isv_svn {
                return Err(TransportError::Attestation(format!(
                    "isv_svn {isv_svn} below min {}",
                    policy.min_isv_svn
                )));
            }
        }
    } else if !policy.dev_accept_any_measurement && !policy.dev_fallback_if_unattested {
        // No SGX hardware quote (gramine-direct / bare) under a strict policy with
        // no unattested fallback.
        return Err(TransportError::Attestation(
            "enclave is unattested (no SGX quote) but policy is strict".to_string(),
        ));
    }

    Ok(AttestedPeerKeys {
        recipient_x25519: *recipient_x25519_pub,
        attestation_pub: *attestation_pub,
        noise_static_pub: *noise_static_pub,
    })
}

/// Verify the quote and return the enclave Noise static public key to pin (the
/// connect path). Thin wrapper over [`verify_peer_quote`].
fn verify_quote(quote: &EnclaveResponse, policy: &QuotePolicy) -> Result<[u8; 32], TransportError> {
    Ok(verify_peer_quote(quote, policy)?.noise_static_pub)
}

/// Verify a per-offer attestation tag — an Ed25519 signature over
/// [`crate::protocol::tribute_offer_attestation_preimage`] — against the enclave's
/// attestation public key (pinned from its quote this session, e.g.
/// [`EnclaveClient::attestation_pub`]). A local verify-then-discard check proving
/// the results were computed inside the attested enclave; the tag is never
/// persisted. Returns a typed error on any mismatch.
pub fn verify_tribute_offer_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    results: &[crate::protocol::TributeOfferResult],
    tag: &[u8],
) -> Result<(), TransportError> {
    use ed25519_dalek::{Signature, VerifyingKey};

    let vk = VerifyingKey::from_bytes(attestation_pub).map_err(|e| {
        TransportError::TributeOfferAttestation(format!("bad attestation key: {e}"))
    })?;
    let sig_bytes: [u8; 64] = tag.try_into().map_err(|_| {
        TransportError::TributeOfferAttestation(format!("bad tag length {}", tag.len()))
    })?;
    let sig = Signature::from_bytes(&sig_bytes);
    let preimage =
        crate::protocol::tribute_offer_attestation_preimage(inputs_canonical_hash, results);
    vk.verify_strict(&preimage, &sig)
        .map_err(|e| TransportError::TributeOfferAttestation(format!("signature invalid: {e}")))
}

/// Verify a Gratis-op attestation tag — an Ed25519 signature over
/// [`crate::protocol::gratis_op_attestation_preimage`] — against the enclave's
/// pinned attestation key. Same verify-then-discard semantics as
/// [`verify_tribute_offer_attestation`]: the tag proves the encrypted-state
/// transition was computed inside the attested enclave and is never persisted.
pub fn verify_gratis_op_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    result: &crate::protocol::GratisOpResult,
    tag: &[u8],
) -> Result<(), TransportError> {
    use ed25519_dalek::{Signature, VerifyingKey};

    let vk = VerifyingKey::from_bytes(attestation_pub)
        .map_err(|e| TransportError::GratisOpAttestation(format!("bad attestation key: {e}")))?;
    let sig_bytes: [u8; 64] = tag.try_into().map_err(|_| {
        TransportError::GratisOpAttestation(format!("bad tag length {}", tag.len()))
    })?;
    let sig = Signature::from_bytes(&sig_bytes);
    let preimage = crate::protocol::gratis_op_attestation_preimage(inputs_canonical_hash, result);
    vk.verify_strict(&preimage, &sig)
        .map_err(|e| TransportError::GratisOpAttestation(format!("signature invalid: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quote::MIN_QUOTE_LEN;

    const RB: usize = 48; // report-body offset inside the quote

    /// Build a Quote response with a correct report_data key binding.
    fn quote_with(
        noise: [u8; 32],
        offer: [u8; 32],
        attest: [u8; 32],
        mrenclave: B256,
        mrsigner: B256,
        isv_svn: u16,
        quote_body: Vec<u8>,
    ) -> EnclaveResponse {
        let mut p = Vec::new();
        p.extend_from_slice(&noise);
        p.extend_from_slice(&offer);
        p.extend_from_slice(&attest);
        EnclaveResponse::Quote {
            mrenclave,
            mrsigner,
            isv_svn,
            report_data: keccak256(&p),
            recipient_x25519_pub: offer,
            attestation_pub: attest,
            noise_static_pub: noise,
            quote_body,
            attestation: "none (test)".to_string(),
        }
    }

    /// gramine-direct/bare: an empty (unattested) quote is accepted only by the
    /// dev policy, and the pinned key is the enclave Noise static key.
    #[test]
    fn dev_policy_accepts_unattested_empty_quote() {
        let q = quote_with([1; 32], [2; 32], [3; 32], B256::ZERO, B256::ZERO, 0, vec![]);
        let pinned = verify_quote(&q, &QuotePolicy::dev_accept_any()).unwrap();
        assert_eq!(pinned, [1u8; 32]);
    }

    /// A strict policy MUST reject an unattested enclave (no SGX quote).
    #[test]
    fn strict_policy_rejects_unattested_empty_quote() {
        let q = quote_with([1; 32], [2; 32], [3; 32], B256::ZERO, B256::ZERO, 0, vec![]);
        let policy = QuotePolicy::new(vec![B256::ZERO], vec![B256::ZERO], 0);
        assert!(verify_quote(&q, &policy).is_err());
    }

    /// Mode gate: `from_genesis_strict` accepts an unattested enclave (empty
    /// quote, gramine-direct dev box) via the fallback, but a REAL quote always
    /// hits the strict DCAP path — without the `dcap` feature the stub rejects it,
    /// proving the strict path is reached (it would also require the allowlist).
    #[test]
    fn from_genesis_strict_fallback_and_real_quote_paths() {
        let strict = QuotePolicy::from_genesis_strict(
            vec![B256::repeat_byte(0xAA)],
            vec![B256::repeat_byte(0xBB)],
            0,
        );

        // Unattested empty quote → accepted via dev_fallback_if_unattested.
        let empty = quote_with([1; 32], [2; 32], [3; 32], B256::ZERO, B256::ZERO, 0, vec![]);
        assert!(
            verify_quote(&empty, &strict).is_ok(),
            "empty quote accepted via unattested fallback"
        );

        // A real (non-empty) quote with consistent measurements + report_data is
        // always strictly verified → reaches DCAP, which errors here (no `dcap`
        // feature / no collateral).
        let (noise, offer, attest) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        let me = B256::repeat_byte(0xAA);
        let ms = B256::repeat_byte(0xBB);
        let mut body = vec![0u8; MIN_QUOTE_LEN];
        body[RB + 64..RB + 96].copy_from_slice(me.as_slice());
        body[RB + 128..RB + 160].copy_from_slice(ms.as_slice());
        let mut p = Vec::new();
        p.extend_from_slice(&noise);
        p.extend_from_slice(&offer);
        p.extend_from_slice(&attest);
        let binding = keccak256(&p);
        body[RB + 320..RB + 352].copy_from_slice(binding.as_slice());
        let q = EnclaveResponse::Quote {
            mrenclave: me,
            mrsigner: ms,
            isv_svn: 0,
            report_data: binding,
            recipient_x25519_pub: offer,
            attestation_pub: attest,
            noise_static_pub: noise,
            quote_body: body,
            attestation: "dcap (test)".to_string(),
        };
        let err = verify_quote(&q, &strict).unwrap_err();
        assert!(
            format!("{err}").contains("DCAP"),
            "real quote under strict policy must reach DCAP verification: {err}"
        );
    }

    /// A valid per-offer attestation tag verifies; tampering with the
    /// results, the inputs hash, the key, or the tag length is rejected.
    #[test]
    fn verify_tribute_offer_attestation_accepts_valid_rejects_tampering() {
        use crate::protocol::{TributeOfferResult, TributeOfferStatus};
        use alloy_primitives::{Address, U256};
        use ed25519_dalek::{Signer, SigningKey};

        let results = vec![TributeOfferResult {
            token_id: B256::repeat_byte(0x11),
            owner: Address::repeat_byte(0x22),
            worldwide_day: 20_240,
            issuance_amount_minor: U256::from(1_000u64),
            issuance_currency: 1,
            nominal_amount_minor: U256::from(2_000u64),
            reference_currency: 2,
            exclude_from_intex_issuance: false,
            tribute_price_minor: U256::from(3u64),
            su_hashes: vec!["0xabc".to_string()],
            wallet_addresses: vec![],
            sra_addresses: vec![],
            status: TributeOfferStatus::Created,
        }];
        let hash = B256::repeat_byte(0xAB);

        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let pk = sk.verifying_key().to_bytes();
        let preimage = crate::protocol::tribute_offer_attestation_preimage(hash, &results);
        let tag = sk.sign(&preimage).to_bytes();

        // Happy path.
        verify_tribute_offer_attestation(&pk, hash, &results, &tag).expect("valid tag verifies");

        // Tampered result → reject.
        let mut tampered = results.clone();
        tampered[0].owner = Address::repeat_byte(0x99);
        assert!(verify_tribute_offer_attestation(&pk, hash, &tampered, &tag).is_err());

        // Tampered inputs hash → reject.
        assert!(
            verify_tribute_offer_attestation(&pk, B256::repeat_byte(0xCD), &results, &tag).is_err()
        );

        // Wrong key → reject.
        let other = SigningKey::from_bytes(&[8u8; 32])
            .verifying_key()
            .to_bytes();
        assert!(verify_tribute_offer_attestation(&other, hash, &results, &tag).is_err());

        // Bad tag length → reject.
        assert!(verify_tribute_offer_attestation(&pk, hash, &results, &[0u8; 10]).is_err());
    }

    /// Pin the REPORT_DATA preimage byte order on the host side. The host
    /// binds `keccak256(noise ‖ recipient ‖ attestation)`; a quote whose
    /// report_data uses any other field order must fail the binding. Mirrors the
    /// enclave's `report_data_preimage_order_is_pinned`.
    #[test]
    fn report_data_preimage_order_is_pinned_host() {
        let (noise, offer, attest) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        // Wrong order (noise ‖ attest ‖ offer) must NOT satisfy the binding.
        let mut wrong = Vec::new();
        wrong.extend_from_slice(&noise);
        wrong.extend_from_slice(&attest);
        wrong.extend_from_slice(&offer);
        let q = EnclaveResponse::Quote {
            mrenclave: B256::ZERO,
            mrsigner: B256::ZERO,
            isv_svn: 0,
            report_data: keccak256(&wrong),
            recipient_x25519_pub: offer,
            attestation_pub: attest,
            noise_static_pub: noise,
            quote_body: vec![],
            attestation: "none (test)".to_string(),
        };
        assert!(
            verify_quote(&q, &QuotePolicy::dev_accept_any()).is_err(),
            "a non-canonical preimage order must fail the report_data binding"
        );
    }

    /// A tampered cleartext public key breaks the report_data binding.
    #[test]
    fn rejects_report_data_binding_mismatch() {
        let mut q = quote_with([1; 32], [2; 32], [3; 32], B256::ZERO, B256::ZERO, 0, vec![]);
        if let EnclaveResponse::Quote {
            noise_static_pub, ..
        } = &mut q
        {
            *noise_static_pub = [9; 32]; // no longer hashes to report_data
        }
        assert!(verify_quote(&q, &QuotePolicy::dev_accept_any()).is_err());
    }

    /// Cleartext measurements that disagree with the real quote are rejected even
    /// under the dev policy (the quote bytes are the source of truth).
    #[test]
    fn rejects_cleartext_measurements_not_matching_quote() {
        let (noise, offer, attest) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        let mut body = vec![0u8; MIN_QUOTE_LEN];
        body[RB + 64..RB + 96].copy_from_slice(&[0xAA; 32]); // quote mrenclave
        body[RB + 128..RB + 160].copy_from_slice(&[0xBB; 32]); // quote mrsigner
        let mut p = Vec::new();
        p.extend_from_slice(&noise);
        p.extend_from_slice(&offer);
        p.extend_from_slice(&attest);
        let binding = keccak256(&p);
        body[RB + 320..RB + 352].copy_from_slice(binding.as_slice()); // quote report_data
                                                                      // cleartext claims mrenclave=CC, but the quote says AA -> reject.
        let q = quote_with(
            noise,
            offer,
            attest,
            B256::from([0xCC; 32]),
            B256::from([0xBB; 32]),
            0,
            body,
        );
        assert!(verify_quote(&q, &QuotePolicy::dev_accept_any()).is_err());
    }
}
