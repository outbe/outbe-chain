//! Fixture material for the L2Registry zk gate that every positive Tribute
//! offer path shares: the deterministic root-signing key of a fixture L2
//! network, one real `FullProof` builder bound to a single offer statement, and
//! the unique draft/SU identifiers an offer needs.
//!
//! ZK verification is mandatory, so a fixture operator can only offer with a
//! real proof for its own caller, host chain, day, currency, amount and draft,
//! signed over the proof's Merkle root by the key the registry stores for its
//! chain. Keeping all of that in one module is what makes the small governed
//! fixtures, the genesis-seeded bulk owners, and the specialized `0xdead`
//! scenario provably use the same key and the same proving recipe.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use alloy_primitives::{keccak256, Address, B256};
use ark_bn254::Fr;
use ark_ff::UniformRand;
use commonware_codec::{DecodeExt, Encode};
use commonware_cryptography::bls12381::primitives::{
    group::{Private, G1, G2},
    ops::{self, sign_message},
    variant::MinSig,
};
use outbe_protocol::primitive::signature::SignatureScheme;
use outbe_protocol::protocol::imt::Imt;
use outbe_protocol::protocol::key::{NftSecret, Signer};
use outbe_protocol::protocol::zk::{Circuit, ProofGenerator};
use outbe_protocol::{Codec, OutbeV1, Suite};
use outbe_protocol_derive::Entity;
use outbe_zk_backend::barretenberg::{init_crs, Barretenberg};
use outbe_zk_canonical::full::{full_circuit_domain, FullProvable};
use outbe_zk_canonical::full_proof::COMBINED_LEN as FULL_PROOF_COMBINED_LEN;
use outbe_zk_canonical::noir::full_proof::FullProof;
use outbe_zk_canonical::INCLUSION_DEPTH;
use rand::{rngs::StdRng, SeedableRng};

/// Must match `outbe_l2registry::api::ZK_MERKLE_ROOT_NAMESPACE`. Kept as a
/// literal so the harness exercises the external signing contract rather than
/// importing the runtime crate.
pub(crate) const ZK_MERKLE_ROOT_NAMESPACE: &[u8] = b"_PSO_CHAIN_COMMITMENT_ROOT";

/// Frozen circuit version selected by the chain's development binding stub.
pub(crate) const FIXTURE_CIRCUIT_VERSION: &str = "1.1.0";

/// The `uint32` circuit selector argument of `offerTribute`.
pub(crate) fn circuit_selector(l2_chain_id: u64) -> u32 {
    u32::try_from(l2_chain_id).expect("sandbox L2 chain id fits the uint32 offer selector")
}

/// The TributeDraft an offer commits to. This mirrors the enclave's
/// `TributeDraftClaim` field for field (`base` is the whole-unit amount and
/// `atto` the raw six-decimal remainder) so the proof's `nft_hash` is the hash
/// the enclave recomputes from the decrypted payload and the cleartext day and
/// currency.
#[derive(Entity)]
struct TributeDraftClaim {
    #[outbe(id_seed)]
    id: B256,
    #[outbe(body, owner, pos = 0)]
    derived_owner: B256,
    #[outbe(body, pos = 1)]
    worldwide_day: u64,
    #[outbe(body, pos = 2)]
    currency: u16,
    #[outbe(body, pos = 3)]
    base: u64,
    #[outbe(body, pos = 4)]
    atto: u64,
    #[outbe(body, pos = 5)]
    su_ids: Vec<B256>,
}

/// One offer's circuit selector and zk calldata fields. The proof, its Merkle
/// root and the root signature are kept as bytes; the CLI path renders the hex
/// the product client takes, the raw-ABI paths use the bytes directly.
pub(crate) struct TributeOfferZk {
    pub tribute_draft_id_hex: String,
    pub su_hash_hex: String,
    pub merkle_root: [u8; 32],
    pub proof: Vec<u8>,
    pub signature: Vec<u8>,
    pub l2_chain_id: u32,
    pub circuit_version: &'static str,
}

impl TributeOfferZk {
    pub fn proof_hex(&self) -> String {
        format!("0x{}", hex::encode(&self.proof))
    }

    pub fn signature_hex(&self) -> String {
        format!("0x{}", hex::encode(&self.signature))
    }

    /// The proof's Merkle root as the `0x`-hex `zkMerkleRoot` ABI argument.
    pub fn merkle_root_hex(&self) -> String {
        format!("0x{}", hex::encode(self.merkle_root))
    }
}

/// Everything one proof is bound to. The harness supplies the caller and the
/// host chain id from chain state, and the draft fields from the exact
/// plaintext the offer encrypts, so the proof matches what the node and the
/// enclave recompute.
pub(crate) struct TributeOfferStatement<'a> {
    /// EVM chain id of the chain that executes the offer; the signature binding.
    pub host_chain_id: u64,
    /// Offer caller; the binding is derived from exactly this address.
    pub caller: Address,
    /// The caller's registered L2 chain id, which selects the circuit.
    pub l2_chain_id: u64,
    pub worldwide_day: u64,
    pub tribute_currency: u16,
    pub amount_base: &'a str,
    pub amount_micro: &'a str,
    pub draft_id: B256,
    pub su_hash: B256,
}

/// Deterministic MinSig root-signing key of the L2 network with `chain_id`.
///
/// One key per fixture chain id, reproducible across processes: the genesis
/// seeding path and the governed registration path must store the same public
/// key that later signs the offer's Merkle root, or the node rejects the offer.
pub(crate) fn root_signing_keypair(chain_id: u64) -> (Private, G2) {
    let mut seed = [0x5b_u8; 32];
    seed[..8].copy_from_slice(&chain_id.to_be_bytes());
    let mut rng = <rand_commonware::rngs::StdRng as rand_commonware::SeedableRng>::from_seed(seed);
    ops::keypair::<_, MinSig>(&mut rng)
}

/// Encoded MinSig G2 public key the registry stores for fixture chain `chain_id`.
pub(crate) fn root_signing_public_key(chain_id: u64) -> Vec<u8> {
    root_signing_keypair(chain_id).1.encode().to_vec()
}

/// Sign `merkle_root` with fixture chain `chain_id`'s registered key, as the L2
/// network would, and return the encoded signature.
pub(crate) fn sign_merkle_root(chain_id: u64, merkle_root: &[u8; 32]) -> Vec<u8> {
    let (private, _) = root_signing_keypair(chain_id);
    sign_message::<MinSig>(&private, ZK_MERKLE_ROOT_NAMESPACE, merkle_root)
        .encode()
        .to_vec()
}

/// Positive control for a signed root: decode `public_key` (the key the registry
/// actually stores) and `signature` as MinSig group elements and verify the
/// signature over `merkle_root` under [`ZK_MERKLE_ROOT_NAMESPACE`]. This is what
/// the node's offer gate does, so a fixture can prove its key material agrees
/// with chain state before it relies on an admitted offer.
pub(crate) fn verify_merkle_root(
    public_key: &[u8],
    merkle_root: &[u8; 32],
    signature: &[u8],
) -> bool {
    let Ok(public) = G2::decode(public_key) else {
        return false;
    };
    let Ok(signature) = G1::decode(signature) else {
        return false;
    };
    ops::verify_message::<MinSig>(&public, ZK_MERKLE_ROOT_NAMESPACE, merkle_root, &signature)
        .is_ok()
}

/// Fresh, unique field-canonical `(draft_id, su_hash)` pair for one offer.
///
/// Both must be new for every submission: the host marks SU hashes as used, and
/// a repeated draft id would collide with an earlier Tribute draft. The tag
/// keeps the ids attributable to the path that produced them.
pub(crate) fn offer_identifiers(tag: &str, caller: Address, worldwide_day: u32) -> (B256, B256) {
    static ORDINAL: AtomicU64 = AtomicU64::new(0);
    let ordinal = ORDINAL.fetch_add(1, Ordering::Relaxed);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the unix epoch")
        .as_millis();
    let entropy = format!("{tag}:{caller:#x}:{worldwide_day}:{ordinal}:{millis}");
    let mut rng = StdRng::from_seed(keccak256(entropy.as_bytes()).0);
    (
        B256::from(field_bytes(&Fr::rand(&mut rng))),
        B256::from(field_bytes(&Fr::rand(&mut rng))),
    )
}

/// Prove one Tribute offer statement and sign its Merkle root.
///
/// This is the single proving recipe the harness uses: the witness is the
/// TributeDraftCommitment claim above against the (empty) perpetual commitment
/// tree, and the combined proof carries the four public inputs the node decodes.
/// Barretenberg's prover is process-global and serialized internally, so
/// concurrent callers queue rather than race.
pub(crate) fn prove_tribute_offer(statement: TributeOfferStatement<'_>) -> TributeOfferZk {
    let TributeOfferStatement {
        host_chain_id,
        caller,
        l2_chain_id,
        worldwide_day,
        tribute_currency,
        amount_base,
        amount_micro,
        draft_id,
        su_hash,
    } = statement;
    let base = parse_amount(amount_base, "amount_base");
    let atto = parse_amount(amount_micro, "amount_micro");

    // Use the same host-chain-gated binding lookup as offer admission.
    assert!(
        outbe_l2registry::api::l2_circuits(host_chain_id, l2_chain_id)
            .iter()
            .any(|entry| entry.version == FIXTURE_CIRCUIT_VERSION),
        "L2 chain {l2_chain_id:#x} has no circuit binding for {FIXTURE_CIRCUIT_VERSION} \
         on host {host_chain_id}; the development stub requires the Devnet host"
    );

    // CRS setup uses a blocking download/read path. Generate on a plain thread
    // rather than inside cucumber's Tokio runtime.
    let started = std::time::Instant::now();
    let zk = std::thread::spawn(move || {
        init_crs().expect("pinned CRS initializes for e2e proof generation");

        let mut rng = StdRng::from_seed([9; 32]);
        let (secret, public_key) = <OutbeV1 as Suite>::Signature::keypair(&mut rng);
        let nonce = Fr::rand(&mut rng);
        let derived_owner = OutbeV1::derive_owner(&public_key, nonce).expect("derive owner");
        let draft = TributeDraftClaim {
            id: draft_id,
            derived_owner: B256::from(field_bytes(&derived_owner)),
            worldwide_day,
            currency: tribute_currency,
            base,
            atto,
            su_ids: vec![su_hash],
        };
        let binding = OutbeV1::binding(&caller.into_array(), draft_id.as_ref(), host_chain_id)
            .expect("derive offer binding");
        let signer = Signer::from_secret(NftSecret::new(secret), nonce).expect("draft signer");
        let path = Imt::<OutbeV1>::new(full_circuit_domain(), Fr::from(0u64), INCLUSION_DEPTH)
            .expect("empty commitment tree")
            .empty_inclusion_path(0);
        let (witness, public) = draft
            .derive_full_witness(&mut rng, &signer, binding, &path)
            .expect("derive full-proof witness");
        let proof = ProofGenerator::<OutbeV1, FullProof>::generate(
            &Barretenberg::default(),
            &witness,
            &public,
        )
        .expect("generate the offer FullProof");

        let public_inputs = <FullProof as Circuit<OutbeV1>>::public_inputs(&public);
        assert_eq!(public_inputs.len(), 4);
        let merkle_root = field_bytes(&public_inputs[3]);
        let mut combined = Vec::with_capacity(FULL_PROOF_COMBINED_LEN);
        combined.extend_from_slice(&(public_inputs.len() as u32).to_be_bytes());
        for value in public_inputs {
            combined.extend_from_slice(&field_bytes(&value));
        }
        for field in proof.proof {
            combined.extend_from_slice(&field);
        }
        assert_eq!(combined.len(), FULL_PROOF_COMBINED_LEN);

        TributeOfferZk {
            tribute_draft_id_hex: format!("0x{}", hex::encode(draft_id)),
            su_hash_hex: format!("0x{}", hex::encode(su_hash)),
            merkle_root,
            proof: combined,
            signature: sign_merkle_root(l2_chain_id, &merkle_root),
            l2_chain_id: circuit_selector(l2_chain_id),
            circuit_version: FIXTURE_CIRCUIT_VERSION,
        }
    })
    .join()
    .expect("e2e proof generation thread");
    // Proving is serialized by Barretenberg's process-global prover, so the
    // capacity population's wall time is dominated by this step; report it.
    eprintln!(
        "E2E_TRIBUTE_PROOF caller={caller:#x} l2_chain_id={l2_chain_id} host_chain_id={host_chain_id} wwd={worldwide_day} currency={tribute_currency} base={base} micro={atto} proving_ms={}",
        started.elapsed().as_millis(),
    );
    zk
}

/// The offer's declared amounts, as the enclave parses them.
fn parse_amount(value: &str, what: &'static str) -> u64 {
    value
        .parse::<u64>()
        .unwrap_or_else(|_| panic!("{what} must be a canonical whole u64, got {value:?}"))
}

fn field_bytes(field: &Fr) -> [u8; 32] {
    OutbeV1::field_to_be_bytes(field)
        .try_into()
        .expect("BN254 field encoding is 32 bytes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_root_signing_key_is_deterministic_and_chain_specific() {
        let chain_id = 0xE2E0_0001;
        assert_eq!(
            root_signing_public_key(chain_id),
            root_signing_public_key(chain_id)
        );
        assert_ne!(
            root_signing_public_key(chain_id),
            root_signing_public_key(chain_id + 1),
            "each fixture L2 chain must sign roots with its own registered key"
        );
        assert_eq!(root_signing_public_key(chain_id).len(), 96);
    }

    #[test]
    fn fixture_root_signature_verifies_under_the_registered_key_only() {
        let chain_id = 0xE2E1_0000;
        let root = [7u8; 32];
        let signature = sign_merkle_root(chain_id, &root);
        assert_eq!(signature.len(), 48);
        assert!(verify_merkle_root(
            &root_signing_public_key(chain_id),
            &root,
            &signature
        ));
        assert!(
            !verify_merkle_root(&root_signing_public_key(chain_id + 1), &root, &signature),
            "another fixture chain's key must not verify this root"
        );
        assert!(!verify_merkle_root(&[], &root, &signature));
        assert!(!verify_merkle_root(
            &root_signing_public_key(chain_id),
            &root,
            &[]
        ));
    }

    #[test]
    fn offer_identifiers_are_unique_per_offer() {
        let caller = Address::repeat_byte(0x11);
        let first = offer_identifiers("test", caller, 20_260_729);
        let second = offer_identifiers("test", caller, 20_260_729);
        assert_ne!(first.0, second.0, "draft ids must not repeat");
        assert_ne!(first.1, second.1, "SU hashes must not repeat");
        assert_ne!(first.0, first.1);
        for id in [first.0, first.1, second.0, second.1] {
            outbe_protocol::codec::field_from_be_bytes_canonical::<Fr>(
                id.as_slice(),
                "fixture identifier",
            )
            .expect("draft and SU identifiers must encode as canonical circuit fields");
        }
    }

    #[test]
    #[ignore = "generates and verifies a real Barretenberg FullProof"]
    fn proven_offer_is_a_valid_full_proof_for_its_statement() {
        use outbe_zk_backend::barretenberg::verify_circuit;
        use outbe_zk_canonical::full_proof::{
            alloy::PublicInputs, decode_public_inputs as decode_full_proof_public_inputs,
        };

        let caller = Address::repeat_byte(0x44);
        let (draft_id, su_hash) = offer_identifiers("test", caller, 20_260_729);
        let zk = prove_tribute_offer(TributeOfferStatement {
            host_chain_id: outbe_primitives::chain::DEVNET_CHAIN_ID,
            caller,
            l2_chain_id: 0xE2E0_0001,
            worldwide_day: 20_260_729,
            tribute_currency: 840,
            amount_base: "100",
            amount_micro: "410000",
            draft_id,
            su_hash,
        });
        let proof = hex::decode(zk.proof_hex().trim_start_matches("0x")).expect("proof hex");
        let public: PublicInputs = decode_full_proof_public_inputs(&proof)
            .expect("public inputs decode")
            .try_into()
            .expect("Alloy public inputs");
        assert!(verify_circuit::<FullProof>(&proof).expect("proof verifier succeeds"));
        assert_eq!(public.merkle_root, zk.merkle_root);
        assert_eq!(zk.l2_chain_id, circuit_selector(0xE2E0_0001));
        assert_eq!(zk.circuit_version, FIXTURE_CIRCUIT_VERSION);
    }
}
