//! Host-side TEE enclave client for tribute-offer decryption.
//!
//! When the node is started with `--tee-enclave-socket`, [`init_enclave_client`]
//! connects to the enclave sidecar (attesting its quote + pinning its Noise-IK
//! static key) and installs a process-global client. Every offer decryption then
//! routes through the enclave via [`process_tribute_offer_batch_via_enclave`] — the offer
//! key exists only inside the enclave, and there is no in-process key path. The L1↔L2 linkage
//! (`creator`, `tribute_draft_id`) is parsed
//! and validated inside the enclave but withheld from the host (Enclave Return
//! Rule); the public draft fields are returned and used to issue the tribute.
//!
//! Determinism: every node's enclave holds the same shared offer key, so the same
//! ciphertext decrypts identically on all validators (re-execution agrees). The
//! enclave call is a **blocking** UDS round-trip made straight from the precompile
//! path — it never holds a `StorageHandle` across an await and never spawns a
//! thread (the `StorageHandle` `!Send` constraint). A dead sidecar surfaces as a
//! typed `tee_sidecar_unavailable` error (the offer reverts).

use std::path::Path;

use alloy_primitives::B256;
use outbe_tee::protocol::{
    EnclaveRequest, EnclaveResponse, EncryptedTributeOffer, TributeOfferResult,
};
use outbe_tee::{verify_tribute_offer_attestation, EnclaveClient, QuotePolicy};

/// True once an enclave client is installed. Offers always route through the
/// enclave (single path); when no client is configured, `offerTribute` reverts
/// with a typed `tee_sidecar_unavailable` error. Delegates to the process-global
/// enclave client in `outbe-tee` (shared with the TEE registry seal).
pub fn is_enclave_configured() -> bool {
    outbe_tee::is_enclave_configured()
}

/// Connect to the enclave sidecar at `socket` under the host attestation
/// `policy`, verify it answers an encrypted request, and install the global
/// offer-decryption client. Called once at node startup. `policy` is built from
/// the genesis `teePolicy`: a configured policy strictly verifies the
/// enclave's DCAP quote (measurement allowlist + signature) on hardware, while an
/// empty/dev policy accepts an unattested `gramine-direct` enclave (still
/// enforcing the REPORT_DATA key binding). This brings the offer-decrypt connect
/// to parity with the consensus DKG/bootstrap connect sites.
pub fn init_enclave_client(socket: &Path, policy: &QuotePolicy) -> eyre::Result<()> {
    // A `host:port` endpoint connects over TCP (enclave under Gramine); a path
    // connects over the Unix domain socket (native sidecar).
    let endpoint = socket
        .to_str()
        .ok_or_else(|| eyre::eyre!("TEE enclave endpoint is not valid UTF-8"))?;
    let mut client = EnclaveClient::connect_endpoint(endpoint, policy)
        .map_err(|e| eyre::eyre!("TEE enclave connect at {} failed: {e}", socket.display()))?;
    // Verify the encrypted channel works before installing the client.
    match client.request(&EnclaveRequest::GetPublicKeys) {
        Ok(EnclaveResponse::PublicKeys { .. }) => {}
        Ok(other) => {
            return Err(eyre::eyre!(
                "unexpected enclave handshake response: {other:?}"
            ))
        }
        Err(e) => return Err(eyre::eyre!("enclave GetPublicKeys failed: {e}")),
    }
    // Report the real attestation status (derived from the enclave's quote), not
    // a hardcoded assumption: a real SGX quote yields non-zero measurements;
    // gramine-direct/bare yields an unattested enclave accepted only by the dev
    // policy. NOTE: with the dev policy an unattested enclave IS accepted — this
    // log is the operator's signal that the deployment is not confidential.
    let mode = client.attestation_label();
    if client.is_hardware_attested() {
        let (mrenclave, mrsigner, isv_svn) = client.measurements();
        tracing::info!(
            attestation = %mode, %mrenclave, %mrsigner, isv_svn,
            "TEE enclave is SGX hardware-attested (real DCAP quote)"
        );
    } else {
        tracing::warn!(
            attestation = %mode,
            "TEE enclave is UNATTESTED: mode is `{mode}`, so the enclave produced no \
             SGX quote and attestation CANNOT be verified — accepted under the dev policy. \
             NOT confidential. Use gramine-sgx + a strict QuotePolicy for production."
        );
    }
    outbe_tee::install_enclave_client(client).map_err(|e| eyre::eyre!(e))?;
    Ok(())
}

/// Process a batch of offers through the enclave: it decrypts (offer key stays in
/// SGX), applies the node-supplied oracle price, computes economics + Poseidon
/// `token_id`, and returns the public `TributeOfferResult[]`. The host then issues
/// the Tributes from the returned fields (no host recompute).
///
/// The host recomputes `inputs_canonical_hash` from the request it sent and
/// compares it to the enclave's — a mismatch is enclave non-determinism
/// (`tee_enclave_nondeterminism`). It then verifies the per-offer `attestation_tag`
/// (an Ed25519 signature over the inputs hash + results) against the attestation
/// key pinned from the enclave's quote, proving the results were produced inside
/// the attested enclave (`tee_offer_attestation_invalid` on failure); the tag is
/// then discarded (never written to chain state). Both checks live in
/// [`validate_tribute_offer_batch_response`] so they are unit-testable without a sidecar.
pub fn process_tribute_offer_batch_via_enclave(
    offers: &[EncryptedTributeOffer],
) -> eyre::Result<Vec<TributeOfferResult>> {
    // Route through the process-global enclave client (shared with the TEE registry
    // seal). Pin the attestation key from this session's verified quote before the
    // call. `None` means no client is configured → typed `tee_sidecar_unavailable`.
    let (attestation_pub, response) = outbe_tee::try_with_enclave(|client| {
        let attestation_pub = client.attestation_pub();
        let response = client.request(&EnclaveRequest::ProcessTributeOfferBatch {
            offers: offers.to_vec(),
        });
        (attestation_pub, response)
    })
    .ok_or_else(|| eyre::eyre!("enclave client not configured (tee_sidecar_unavailable)"))?;
    let response = response.map_err(|e| {
        eyre::eyre!("enclave ProcessTributeOfferBatch failed (tee_sidecar_unavailable): {e}")
    })?;

    match response {
        EnclaveResponse::TributeOfferBatch {
            results,
            inputs_canonical_hash,
            attestation_tag,
        } => validate_tribute_offer_batch_response(
            offers,
            results,
            inputs_canonical_hash,
            &attestation_pub,
            &attestation_tag,
        ),
        other => Err(eyre::eyre!("unexpected enclave response: {other:?}")),
    }
}

/// Validate the enclave's `TributeOfferBatch` response: (1) the canonical-inputs hash
/// equals the host's recompute over the exact request it sent (non-determinism
/// detector → `tee_enclave_nondeterminism`); (2) the per-offer attestation tag
/// verifies against the pinned attestation key (→ `tee_offer_attestation_invalid`).
/// Returns the results on success. Pure (no transport) so it is unit-testable.
fn validate_tribute_offer_batch_response(
    offers: &[EncryptedTributeOffer],
    results: Vec<TributeOfferResult>,
    inputs_canonical_hash: B256,
    attestation_pub: &[u8; 32],
    attestation_tag: &[u8],
) -> eyre::Result<Vec<TributeOfferResult>> {
    let expected = outbe_tee::protocol::inputs_canonical_hash(offers);
    if inputs_canonical_hash != expected {
        return Err(eyre::eyre!(
            "tee_enclave_nondeterminism: inputs_canonical_hash mismatch"
        ));
    }
    verify_tribute_offer_attestation(
        attestation_pub,
        inputs_canonical_hash,
        &results,
        attestation_tag,
    )
    .map_err(|e| eyre::eyre!("tee_offer_attestation_invalid: {e}"))?;
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, U256};

    fn sample_tribute_offer() -> EncryptedTributeOffer {
        EncryptedTributeOffer {
            owner: Address::repeat_byte(0x11),
            cipher_text: vec![1, 2, 3, 4],
            nonce: vec![0u8; 12],
            ephemeral_pubkey: U256::from(7u64),
            reference_currency: 840,
            tribute_price_minor: U256::from(1_000u64),
        }
    }

    /// A returned `inputs_canonical_hash` that disagrees with the host's
    /// recompute is enclave non-determinism → `tee_enclave_nondeterminism`. The
    /// hash check runs before attestation, so a bogus tag is irrelevant here.
    #[test]
    fn validate_rejects_inputs_canonical_hash_divergence() {
        let offers = vec![sample_tribute_offer()];
        let wrong_hash = B256::repeat_byte(0xFF);
        // Sanity: the wrong hash really differs from the canonical recompute.
        assert_ne!(
            wrong_hash,
            outbe_tee::protocol::inputs_canonical_hash(&offers)
        );

        let err =
            validate_tribute_offer_batch_response(&offers, Vec::new(), wrong_hash, &[0u8; 32], &[])
                .expect_err("hash divergence must be rejected");
        assert!(
            err.to_string().contains("tee_enclave_nondeterminism"),
            "unexpected error: {err}"
        );
    }
}
