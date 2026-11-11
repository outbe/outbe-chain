//! Development fixture for one Tribute offer on a development network.
//!
//! `outbe-cli tribute offer` needs a Demo Tribute proof, its Merkle root and a
//! signature over that root by the root-signing key of a registered L2 network.
//! No operator tool produces them. This binary does, with a deterministic L2
//! key per chain id. It is for development networks only: the L2 key is public.
//!
//! - `l2-key` prints the L2 public key for the L2Registry registration payload.
//! - `offer` prints the proof, the Merkle root, the signature and fresh offer
//!   identifiers as JSON.

use std::time::{SystemTime, UNIX_EPOCH};

use alloy_primitives::{keccak256, Address, B256};
use ark_bn254::Fr;
use ark_ff::UniformRand;
use clap::{Parser, Subcommand};
use commonware_codec::Encode;
use commonware_cryptography::bls12381::primitives::{
    group::{Private, G2},
    ops::{self, sign_message},
    variant::MinSig,
};
use eyre::{ensure, eyre, Result, WrapErr};
use outbe_zk::claim::TributeDraftClaim;
use outbe_zk::demo_tribute::{
    demo_tribute_domain, DemoTributeProvable, COMBINED_LEN as DEMO_TRIBUTE_COMBINED_LEN,
};
use outbe_zk::noir::demo_tribute::DemoTribute;
use outbe_zk::primitive::hash::{self, derive_owner};
use outbe_zk::primitive::signature;
use outbe_zk::protocol::imt::Imt;
use outbe_zk::protocol::key::{NftSecret, Signer};
use outbe_zk::protocol::zk::{Circuit, ProofGenerator};
use outbe_zk::INCLUSION_DEPTH;
use outbe_zk_backend::barretenberg::{Barretenberg, BarretenbergHandler};
use rand::{rngs::StdRng, SeedableRng};

/// Signature namespace of the L2 commitment root. It must equal
/// `outbe_l2registry::api::ZK_MERKLE_ROOT_NAMESPACE`.
const ZK_MERKLE_ROOT_NAMESPACE: &[u8] = b"_PSO_CHAIN_COMMITMENT_ROOT";

#[derive(Parser)]
#[command(about = "Development fixture: L2 key, proof and signature for one Tribute offer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the deterministic L2 root-signing public key (EIP-2537 G2, 256 bytes).
    L2Key {
        #[arg(long)]
        l2_chain_id: u64,
    },
    /// Prove one offer statement and sign its Merkle root.
    Offer(OfferArgs),
}

#[derive(clap::Args)]
struct OfferArgs {
    /// EVM chain id of the chain that executes the offer.
    #[arg(long)]
    host_chain_id: u64,
    /// Transaction signer of the offer. The proof binds this address.
    #[arg(long)]
    caller: Address,
    /// Registered L2 chain id that selects the circuit.
    #[arg(long)]
    l2_chain_id: u64,
    /// Circuit version that must be enabled for the L2 chain id on the host.
    #[arg(long, default_value = "1.1.0")]
    circuit_version: String,
    /// WorldwideDay of the offer, for example 20261109.
    #[arg(long)]
    worldwide_day: u64,
    /// Issuance currency, ISO 4217 numeric.
    #[arg(long, default_value_t = 840)]
    currency: u16,
    /// Issuance amount in whole units.
    #[arg(long, default_value_t = 100)]
    amount: u64,
    /// Six-decimal remainder of the issuance amount.
    #[arg(long, default_value_t = 0)]
    amount_micro: u64,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::L2Key { l2_chain_id } => {
            let public_key = root_signing_public_key(l2_chain_id)?;
            println!("0x{}", hex::encode(public_key));
            Ok(())
        }
        Command::Offer(args) => {
            let offer = prove_offer(&args)?;
            println!("{}", serde_json::to_string_pretty(&offer)?);
            Ok(())
        }
    }
}

/// Deterministic MinSig root-signing key of the fixture L2 network `chain_id`.
fn root_signing_keypair(chain_id: u64) -> (Private, G2) {
    let mut seed = [0x5b_u8; 32];
    seed[..8].copy_from_slice(&chain_id.to_be_bytes());
    let mut rng = <rand_commonware::rngs::StdRng as rand_commonware::SeedableRng>::from_seed(seed);
    ops::keypair::<_, MinSig>(&mut rng)
}

fn root_signing_public_key(chain_id: u64) -> Result<Vec<u8>> {
    let (_, public) = root_signing_keypair(chain_id);
    Ok(outbe_l2registry::public_key::encode(&public)
        .map_err(|error| eyre!("encode L2 root key as EIP-2537: {error:?}"))?
        .to_vec())
}

fn sign_merkle_root(chain_id: u64, merkle_root: &[u8; 32]) -> Vec<u8> {
    let (private, _) = root_signing_keypair(chain_id);
    sign_message::<MinSig>(&private, ZK_MERKLE_ROOT_NAMESPACE, merkle_root)
        .encode()
        .to_vec()
}

fn field_bytes(field: &Fr) -> Result<[u8; 32]> {
    outbe_zk::codec::field_to_be_bytes(field)
        .try_into()
        .map_err(|_| eyre!("BN254 field encoding is not 32 bytes"))
}

/// Fresh field-canonical `(draft_id, su_hash)` pair. Both must be new for every
/// offer: the chain records used SpendingUnit hashes and draft ids.
fn offer_identifiers(caller: Address, worldwide_day: u64) -> Result<(B256, B256)> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .wrap_err("system clock is before the unix epoch")?
        .as_nanos();
    let entropy = format!("tribute-offer-fixture:{caller:#x}:{worldwide_day}:{nanos}");
    let mut rng = StdRng::from_seed(keccak256(entropy.as_bytes()).0);
    Ok((
        B256::from(field_bytes(&Fr::rand(&mut rng))?),
        B256::from(field_bytes(&Fr::rand(&mut rng))?),
    ))
}

fn prove_offer(args: &OfferArgs) -> Result<serde_json::Value> {
    ensure!(
        args.amount_micro < 1_000_000,
        "amount-micro must be below 1000000"
    );
    ensure!(
        outbe_l2registry::api::l2_circuits(args.host_chain_id, args.l2_chain_id)
            .iter()
            .any(|entry| entry.version == args.circuit_version),
        "L2 chain {} has no circuit binding for version {} on host chain {}",
        args.l2_chain_id,
        args.circuit_version,
        args.host_chain_id
    );
    let selector = u32::try_from(args.l2_chain_id)
        .wrap_err("L2 chain id does not fit the uint32 offer selector")?;

    BarretenbergHandler::init().map_err(|error| eyre!("initialize the pinned CRS: {error}"))?;

    let (draft_id, su_hash) = offer_identifiers(args.caller, args.worldwide_day)?;
    let mut rng = StdRng::from_seed([9; 32]);
    let (secret, public_key) = signature::keypair(&mut rng);
    let nonce = Fr::rand(&mut rng);
    let derived_owner =
        derive_owner(&public_key, nonce).map_err(|error| eyre!("derive owner: {error:?}"))?;
    let draft = TributeDraftClaim {
        id: draft_id,
        derived_owner: B256::from(field_bytes(&derived_owner)?),
        worldwide_day: args.worldwide_day,
        currency: args.currency,
        base: args.amount,
        atto: args.amount_micro,
        su_ids: vec![su_hash],
    };
    let binding = hash::binding(
        &args.caller.into_array(),
        &draft_id.0,
        args.host_chain_id,
        args.l2_chain_id,
    )
    .map_err(|error| eyre!("derive offer binding: {error:?}"))?;
    let signer = Signer::from_secret(NftSecret::new(secret), nonce)
        .map_err(|error| eyre!("build draft signer: {error:?}"))?;
    let path = Imt::new(demo_tribute_domain(), Fr::from(0u64), INCLUSION_DEPTH)
        .map_err(|error| eyre!("build empty commitment tree: {error:?}"))?
        .empty_inclusion_path(0);
    let (witness, public) = draft
        .derive_demo_tribute_witness(&mut rng, &signer, binding, &path)
        .map_err(|error| eyre!("derive demo tribute witness: {error:?}"))?;
    let proof =
        ProofGenerator::<DemoTribute>::generate(&Barretenberg::default(), &witness, &public)
            .map_err(|error| eyre!("generate the Demo Tribute proof: {error:?}"))?;

    let public_inputs = <DemoTribute as Circuit>::public_inputs(&public);
    ensure!(
        public_inputs.len() == 4,
        "Demo Tribute circuit must have four public inputs"
    );
    let merkle_root = field_bytes(&public_inputs[3])?;
    let mut combined = Vec::with_capacity(DEMO_TRIBUTE_COMBINED_LEN);
    combined.extend_from_slice(&4_u32.to_be_bytes());
    for value in &public_inputs {
        combined.extend_from_slice(&field_bytes(value)?);
    }
    for field in proof.proof {
        combined.extend_from_slice(&field);
    }
    ensure!(
        combined.len() == DEMO_TRIBUTE_COMBINED_LEN,
        "combined proof length {} differs from the expected {}",
        combined.len(),
        DEMO_TRIBUTE_COMBINED_LEN
    );

    Ok(serde_json::json!({
        "worldwide_day": args.worldwide_day,
        "caller": format!("{:#x}", args.caller),
        "currency": args.currency,
        "amount": args.amount,
        "amount_micro": args.amount_micro,
        "l2_chain_id": selector,
        "circuit_version": args.circuit_version,
        "tribute_draft_id": format!("0x{}", hex::encode(draft_id)),
        "su_hash": format!("0x{}", hex::encode(su_hash)),
        "zk_merkle_root": format!("0x{}", hex::encode(merkle_root)),
        "zk_proof": format!("0x{}", hex::encode(combined)),
        "signature": format!("0x{}", hex::encode(sign_merkle_root(args.l2_chain_id, &merkle_root))),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonware_codec::DecodeExt;
    use commonware_cryptography::bls12381::primitives::group::G1;

    #[test]
    fn root_signature_verifies_under_the_registered_public_key() {
        let chain_id = 57_005;
        let merkle_root = [7_u8; 32];
        let public_key = root_signing_public_key(chain_id).unwrap();
        assert_eq!(public_key.len(), 256);

        let public = outbe_l2registry::public_key::decode(&public_key).unwrap();
        let signature = G1::decode(sign_merkle_root(chain_id, &merkle_root).as_slice()).unwrap();
        ops::verify_message::<MinSig>(&public, ZK_MERKLE_ROOT_NAMESPACE, &merkle_root, &signature)
            .unwrap();
    }

    #[test]
    fn root_keys_differ_between_chain_ids() {
        assert_ne!(
            root_signing_public_key(57_005).unwrap(),
            root_signing_public_key(57_006).unwrap()
        );
    }

    #[test]
    fn offer_identifiers_are_fresh_for_each_call() {
        let caller = Address::repeat_byte(0x11);
        let first = offer_identifiers(caller, 20_261_109).unwrap();
        let second = offer_identifiers(caller, 20_261_109).unwrap();
        assert_ne!(first, second);
        assert_ne!(first.0, first.1);
    }
}
