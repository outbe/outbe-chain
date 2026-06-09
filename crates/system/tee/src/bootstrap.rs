//! Host-side producer for the one-time `TeeBootstrap` payload (Phase 3b).
//!
//! After the TEE DKG ceremony completes, the host assembles the
//! [`TeeBootstrapPayload`] that the begin-zone `TeeBootstrap` system transaction
//! carries (and that `outbe_evm::begin_block_precompile::run_tee_bootstrap`
//! verifies). This module builds the *unsigned* payload from each enclave's
//! announced identity and the ceremony metadata; each validator then signs
//! [`TeeBootstrapPayload::signing_hash`] with its EVM key and the signature is
//! attached via [`attach_signature`].
//!
//! This crate carries no secret cryptography: the registration fields are the
//! enclave's *public* identity (from its attested quote / `GetPublicKeys`), and
//! signing is performed by the caller's validator EVM key, not here.

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{Address, B256};
use outbe_primitives::tee_bootstrap::{
    TeeBootstrapPayload, TeePolicy, TeeRegistrationBundle, TeeValidatorSignature,
};

use crate::tee_dkg::CeremonyError;

/// One validator enclave's public identity, as collected from its attested quote
/// / `GetPublicKeys` response. `keys_hash` is derived (not supplied) so it always
/// commits to exactly these fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnclaveRegistration {
    pub validator: Address,
    pub recipient_x25519: B256,
    pub attestation_pub: B256,
    pub noise_static_pub: B256,
    pub mrenclave: B256,
    pub mrsigner: B256,
    pub isv_svn: u16,
}

/// Ceremony + epoch metadata bound into the bootstrap payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapParams {
    /// The TEE group offer public key clients encrypt to (derived in-enclave from
    /// the assembled group secret; supplied here as the public value).
    pub tribute_offer_public_key: B256,
    /// Genesis attestation allowlist; the payload's `policy_hash` is derived from
    /// it (`policy.compute_hash()`) so the handler can bind it to the
    /// genesis-seeded `TeeRegistry.policy_hash`.
    pub policy: TeePolicy,
    pub key_epoch: u64,
    pub tribute_offer_epoch: u64,
    pub dkg_transcript_hash: B256,
    pub committee_snapshot_block: u64,
    pub committee_snapshot_hash: B256,
}

/// Build the unsigned bootstrap payload: every registration's `keys_hash` is
/// recomputed from its key material (so the consumer's `computed_keys_hash`
/// check passes), and `validator_signatures` starts empty. Validators sign
/// [`TeeBootstrapPayload::signing_hash`] and attach via [`attach_signature`].
pub fn build_unsigned_bootstrap(
    params: &BootstrapParams,
    registrations: &[EnclaveRegistration],
) -> TeeBootstrapPayload {
    let registrations = registrations
        .iter()
        .map(|reg| {
            let mut bundle = TeeRegistrationBundle {
                validator: reg.validator,
                recipient_x25519: reg.recipient_x25519,
                attestation_pub: reg.attestation_pub,
                noise_static_pub: reg.noise_static_pub,
                mrenclave: reg.mrenclave,
                mrsigner: reg.mrsigner,
                isv_svn: reg.isv_svn,
                keys_hash: B256::ZERO,
            };
            bundle.keys_hash = bundle.computed_keys_hash();
            bundle
        })
        .collect();

    TeeBootstrapPayload {
        // `policy_hash` is derived from the allowlist so it always commits to
        // exactly `policy`; the handler binds it to the genesis-seeded value.
        policy_hash: params.policy.compute_hash(),
        committee_snapshot_hash: params.committee_snapshot_hash,
        committee_snapshot_block: params.committee_snapshot_block,
        key_epoch: params.key_epoch,
        tribute_offer_epoch: params.tribute_offer_epoch,
        dkg_transcript_hash: params.dkg_transcript_hash,
        tribute_offer_public_key: params.tribute_offer_public_key,
        registrations,
        policy: params.policy.clone(),
        validator_signatures: Vec::new(),
    }
}

/// Attach a validator's recoverable ECDSA signature (over the payload's
/// [`TeeBootstrapPayload::signing_hash`]) to the payload.
pub fn attach_signature(
    payload: &mut TeeBootstrapPayload,
    validator: Address,
    signature: [u8; 65],
) {
    payload.validator_signatures.push(TeeValidatorSignature {
        validator,
        signature,
    });
}

/// The gossip surface the bootstrap coordination needs: broadcast an opaque
/// message and receive the next one. Implemented over the consensus P2P channel
/// in the node; an in-memory implementation drives the test.
#[allow(async_fn_in_trait)]
pub trait BootstrapGossip {
    async fn broadcast(&mut self, bytes: Vec<u8>) -> Result<(), CeremonyError>;
    async fn recv(&mut self) -> Option<Vec<u8>>;
}

/// One coordination message: a validator's enclave registration, or its signature
/// over the assembled payload. Fixed-size, opaque to the P2P layer.
enum BootstrapMsg {
    Registration(EnclaveRegistration),
    Signature { validator: Address, sig: [u8; 65] },
}

impl BootstrapMsg {
    fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        match self {
            BootstrapMsg::Registration(r) => {
                buf.push(0);
                buf.extend_from_slice(r.validator.as_slice());
                buf.extend_from_slice(r.recipient_x25519.as_slice());
                buf.extend_from_slice(r.attestation_pub.as_slice());
                buf.extend_from_slice(r.noise_static_pub.as_slice());
                buf.extend_from_slice(r.mrenclave.as_slice());
                buf.extend_from_slice(r.mrsigner.as_slice());
                buf.extend_from_slice(&r.isv_svn.to_be_bytes());
            }
            BootstrapMsg::Signature { validator, sig } => {
                buf.push(1);
                buf.extend_from_slice(validator.as_slice());
                buf.extend_from_slice(sig);
            }
        }
        buf
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, CeremonyError> {
        let err = || CeremonyError::MalformedWire("bootstrap message");
        match bytes.first().ok_or_else(err)? {
            0 if bytes.len() == 1 + 20 + 32 * 5 + 2 => {
                let mut addr = [0u8; 20];
                addr.copy_from_slice(&bytes[1..21]);
                let mut o = 21;
                let b256 = |o: &mut usize| {
                    let v = B256::from_slice(&bytes[*o..*o + 32]);
                    *o += 32;
                    v
                };
                let recipient_x25519 = b256(&mut o);
                let attestation_pub = b256(&mut o);
                let noise_static_pub = b256(&mut o);
                let mrenclave = b256(&mut o);
                let mrsigner = b256(&mut o);
                let isv_svn = u16::from_be_bytes([bytes[o], bytes[o + 1]]);
                Ok(BootstrapMsg::Registration(EnclaveRegistration {
                    validator: Address::from(addr),
                    recipient_x25519,
                    attestation_pub,
                    noise_static_pub,
                    mrenclave,
                    mrsigner,
                    isv_svn,
                }))
            }
            1 if bytes.len() == 1 + 20 + 65 => {
                let validator = Address::from_slice(&bytes[1..21]);
                let mut sig = [0u8; 65];
                sig.copy_from_slice(&bytes[21..86]);
                Ok(BootstrapMsg::Signature { validator, sig })
            }
            _ => Err(err()),
        }
    }
}

/// Coordinate the one-time TEE bootstrap among the consensus committee: every
/// validator broadcasts its enclave registration, all assemble the *identical*
/// unsigned payload (registrations sorted by validator address), every validator
/// signs the resulting [`TeeBootstrapPayload::signing_hash`] with its EVM key,
/// and all collect the committee's signatures. Returns the fully-signed payload —
/// byte-identical on every honest node, so the proposer injects exactly what the
/// verifier (`run_tee_bootstrap`) accepts.
///
/// `committee` is every expected validator address (incl. this node). `sign_hash`
/// signs with this validator's EVM key. The all-honest PoC waits for all
/// `committee.len()` registrations and signatures (production tolerates a
/// supermajority with timeouts on this same loop).
pub async fn run_tee_bootstrap_coordination<G: BootstrapGossip>(
    my_registration: EnclaveRegistration,
    params: &BootstrapParams,
    committee: &BTreeSet<Address>,
    sign_hash: impl Fn(&B256) -> [u8; 65],
    gossip: &mut G,
) -> Result<TeeBootstrapPayload, CeremonyError> {
    let my_validator = my_registration.validator;

    // Round 1: exchange registrations. Signature messages that arrive early are
    // buffered for round 2 so no signature is lost.
    let mut registrations: BTreeMap<Address, EnclaveRegistration> = BTreeMap::new();
    registrations.insert(my_validator, my_registration.clone());
    gossip
        .broadcast(BootstrapMsg::Registration(my_registration).to_bytes())
        .await?;

    let mut early_sigs: BTreeMap<Address, [u8; 65]> = BTreeMap::new();
    while registrations.len() < committee.len() {
        let bytes = gossip
            .recv()
            .await
            .ok_or(CeremonyError::UnexpectedResponse("bootstrap gossip closed"))?;
        match BootstrapMsg::from_bytes(&bytes)? {
            BootstrapMsg::Registration(reg) if committee.contains(&reg.validator) => {
                registrations.insert(reg.validator, reg);
            }
            BootstrapMsg::Signature { validator, sig } if committee.contains(&validator) => {
                early_sigs.insert(validator, sig);
            }
            _ => {}
        }
    }

    // Assemble the deterministic unsigned payload (registrations sorted by address).
    let ordered: Vec<EnclaveRegistration> = registrations.into_values().collect();
    let mut payload = build_unsigned_bootstrap(params, &ordered);
    let signing_hash = payload.signing_hash();

    // Round 2: exchange signatures (folding in any buffered early ones).
    let mut signatures: BTreeMap<Address, [u8; 65]> = early_sigs;
    signatures.insert(my_validator, sign_hash(&signing_hash));
    gossip
        .broadcast(
            BootstrapMsg::Signature {
                validator: my_validator,
                sig: signatures[&my_validator],
            }
            .to_bytes(),
        )
        .await?;

    while signatures.len() < committee.len() {
        let bytes = gossip
            .recv()
            .await
            .ok_or(CeremonyError::UnexpectedResponse("bootstrap gossip closed"))?;
        if let BootstrapMsg::Signature { validator, sig } = BootstrapMsg::from_bytes(&bytes)? {
            if committee.contains(&validator) {
                signatures.insert(validator, sig);
            }
        }
    }

    for (validator, sig) in signatures {
        attach_signature(&mut payload, validator, sig);
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::signature::hazmat::PrehashSigner;
    use k256::ecdsa::SigningKey;
    use outbe_primitives::tee_bootstrap::recover_signer;

    fn evm_address(key: &SigningKey) -> Address {
        let point = key.verifying_key().to_encoded_point(false);
        Address::from_slice(&alloy_primitives::keccak256(&point.as_bytes()[1..])[12..])
    }

    fn sign65(key: &SigningKey, prehash: &B256) -> [u8; 65] {
        let (sig, recid): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
            key.sign_prehash(prehash.as_slice()).expect("sign");
        let mut out = [0u8; 65];
        out[..64].copy_from_slice(sig.to_bytes().as_slice());
        out[64] = recid.to_byte();
        out
    }

    fn params() -> BootstrapParams {
        BootstrapParams {
            tribute_offer_public_key: B256::repeat_byte(0x70),
            policy: TeePolicy {
                allowed_mrsigner: vec![B256::repeat_byte(0x50)],
                allowed_mrenclave: vec![B256::repeat_byte(0x60)],
                min_isv_svn: 1,
            },
            key_epoch: 1,
            tribute_offer_epoch: 1,
            dkg_transcript_hash: B256::repeat_byte(0x72),
            committee_snapshot_block: 9,
            committee_snapshot_hash: B256::repeat_byte(0x73),
        }
    }

    /// The producer's output must satisfy exactly the checks the consumer
    /// (`run_tee_bootstrap`) runs: per-registration `keys_hash` integrity, each
    /// signature recovering to its declared validator over `signing_hash`, and a
    /// clean codec round-trip.
    #[test]
    fn produced_bootstrap_passes_consumer_checks() {
        let keys: Vec<SigningKey> = (1u8..=4)
            .map(|s| SigningKey::from_slice(&[s; 32]).expect("scalar"))
            .collect();

        let registrations: Vec<EnclaveRegistration> = keys
            .iter()
            .enumerate()
            .map(|(i, key)| EnclaveRegistration {
                validator: evm_address(key),
                recipient_x25519: B256::repeat_byte(0x20 + i as u8),
                attestation_pub: B256::repeat_byte(0x30 + i as u8),
                noise_static_pub: B256::repeat_byte(0x40 + i as u8),
                mrenclave: B256::repeat_byte(0x50),
                mrsigner: B256::repeat_byte(0x60),
                isv_svn: 1,
            })
            .collect();

        let mut payload = build_unsigned_bootstrap(&params(), &registrations);

        // Sign with each validator's EVM key over the signed body.
        let signing_hash = payload.signing_hash();
        for key in &keys {
            attach_signature(&mut payload, evm_address(key), sign65(key, &signing_hash));
        }

        // Consumer check 1: every registration's keys_hash recomputes.
        for reg in &payload.registrations {
            assert_eq!(reg.computed_keys_hash(), reg.keys_hash);
        }
        // Consumer check 2: every signature recovers to its declared validator.
        let hash = payload.signing_hash();
        for sig in &payload.validator_signatures {
            assert_eq!(
                recover_signer(&hash, &sig.signature).unwrap(),
                sig.validator
            );
        }
        // Registrations and signers describe the same validator set.
        let regs: std::collections::BTreeSet<Address> =
            payload.registrations.iter().map(|r| r.validator).collect();
        let signers: std::collections::BTreeSet<Address> = payload
            .validator_signatures
            .iter()
            .map(|s| s.validator)
            .collect();
        assert_eq!(regs, signers);

        // Consumer check 3: clean wire round-trip.
        let encoded = payload.encode().expect("encode");
        assert_eq!(TeeBootstrapPayload::decode(&encoded).unwrap(), payload);
    }

    #[test]
    fn signatures_do_not_change_the_signing_hash() {
        let registrations = vec![EnclaveRegistration {
            validator: Address::repeat_byte(0x11),
            recipient_x25519: B256::repeat_byte(0x21),
            attestation_pub: B256::repeat_byte(0x22),
            noise_static_pub: B256::repeat_byte(0x23),
            mrenclave: B256::repeat_byte(0x24),
            mrsigner: B256::repeat_byte(0x25),
            isv_svn: 2,
        }];
        let mut payload = build_unsigned_bootstrap(&params(), &registrations);
        let before = payload.signing_hash();
        attach_signature(&mut payload, Address::repeat_byte(0x11), [0xAB; 65]);
        assert_eq!(
            payload.signing_hash(),
            before,
            "signatures are excluded from the signed body"
        );
    }

    /// In-memory broadcast bus for the coordination test.
    struct Bus {
        senders: Vec<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>,
        my_index: usize,
        receiver: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    }

    impl BootstrapGossip for Bus {
        async fn broadcast(&mut self, bytes: Vec<u8>) -> Result<(), CeremonyError> {
            for (i, tx) in self.senders.iter().enumerate() {
                if i != self.my_index {
                    let _ = tx.send(bytes.clone());
                }
            }
            Ok(())
        }
        async fn recv(&mut self) -> Option<Vec<u8>> {
            self.receiver.recv().await
        }
    }

    fn registration(validator: Address, salt: u8) -> EnclaveRegistration {
        EnclaveRegistration {
            validator,
            recipient_x25519: B256::repeat_byte(salt),
            attestation_pub: B256::repeat_byte(salt.wrapping_add(1)),
            noise_static_pub: B256::repeat_byte(salt.wrapping_add(2)),
            mrenclave: B256::repeat_byte(0x50),
            mrsigner: B256::repeat_byte(0x60),
            isv_svn: 1,
        }
    }

    /// The committee coordination must produce a byte-identical, fully-signed
    /// payload on every node that the consumer (`run_tee_bootstrap`) accepts.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn bootstrap_coordination_produces_identical_signed_payload() {
        use tokio::sync::mpsc;
        let n = 4usize;
        let keys: Vec<SigningKey> = (1u8..=4)
            .map(|s| SigningKey::from_slice(&[s; 32]).unwrap())
            .collect();
        let addrs: Vec<Address> = keys.iter().map(evm_address).collect();
        let committee: BTreeSet<Address> = addrs.iter().copied().collect();
        let regs: Vec<EnclaveRegistration> = addrs
            .iter()
            .enumerate()
            .map(|(i, &v)| registration(v, 0x20 + i as u8))
            .collect();
        let params = params();

        let mut senders = Vec::new();
        let mut receivers = Vec::new();
        for _ in 0..n {
            let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
            senders.push(tx);
            receivers.push(rx);
        }

        let mut tasks = Vec::new();
        for i in 0..n {
            let reg = regs[i].clone();
            let key = keys[i].clone();
            let params = params.clone();
            let committee = committee.clone();
            let mut gossip = Bus {
                senders: senders.clone(),
                my_index: i,
                receiver: receivers.remove(0),
            };
            tasks.push(tokio::spawn(async move {
                run_tee_bootstrap_coordination(
                    reg,
                    &params,
                    &committee,
                    |h| sign65(&key, h),
                    &mut gossip,
                )
                .await
            }));
        }
        drop(senders);

        let mut payloads = Vec::new();
        for t in tasks {
            payloads.push(t.await.unwrap().expect("coordination completes"));
        }

        // Byte-identical on every node.
        let encoded0 = payloads[0].encode().unwrap();
        for p in &payloads {
            assert_eq!(
                p.encode().unwrap(),
                encoded0,
                "all nodes produce identical payload"
            );
        }

        // Passes the consumer checks.
        let p = &payloads[0];
        assert_eq!(p.registrations.len(), n);
        assert_eq!(p.validator_signatures.len(), n);
        let hash = p.signing_hash();
        for sig in &p.validator_signatures {
            assert_eq!(
                recover_signer(&hash, &sig.signature).unwrap(),
                sig.validator
            );
            assert!(committee.contains(&sig.validator));
        }
        for reg in &p.registrations {
            assert_eq!(reg.computed_keys_hash(), reg.keys_hash);
        }
    }
}
