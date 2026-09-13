use super::call_u256;

use crate::abi::ITeeRegistry;
use crate::rpc::Rpc;

use alloy_primitives::keccak256;

use alloy_primitives::B256;

use alloy_sol_types::SolCall;

use eyre::Result;
use eyre::WrapErr;
use k256::ecdsa::signature::hazmat::PrehashSigner as _;

use outbe_primitives::tee_attestation_v1::NodeIdV1;

use outbe_primitives::tee_attestation_v1::TeePolicyV1;

use outbe_primitives::tee_attestation_v1::ValidatorNodeBindingV1;
use outbe_primitives::tee_attestation_v1::ENCLAVE_ID_DOMAIN_V1;

use outbe_tee::protocol::EnclaveRequest;
use outbe_tee::protocol::EnclaveResponse;

use outbe_tee::EnclaveClient;

use std::fs;

use zeroize::Zeroizing;

const DEV_NODE_HOST_DOMAIN_V1: &[u8] = b"outbe/tee/dev-node-host/v1";

/// Query the enclave's offer recipient + identity public keys and (optionally)
/// diff the permanent offer key against the on-chain registry. Before readiness
/// the recipient is onboarding-only and cannot be compared with chain state.
pub(super) async fn pubkey(
    client: &(impl Rpc + Sync),
    enclave_socket: &str,
    diff_chain: bool,
) -> Result<()> {
    let mut enclave = EnclaveClient::connect_endpoint(enclave_socket)
        .map_err(|e| eyre::eyre!("connect enclave at {enclave_socket}: {e}"))?;
    let label = enclave.attestation_label().to_string();
    let (mrenclave, mrsigner, isv_svn) = enclave.measurements();
    let hardware_quote_type_reported = enclave.is_hardware_attested();
    let (offer_key_ready, offer_pub, tee_bls_pub) =
        match enclave.request(&EnclaveRequest::GetPublicKeys) {
            Ok(EnclaveResponse::PublicKeys {
                offer_key_ready,
                recipient_x25519_pub,
                tee_bls_pub,
                ..
            }) => (offer_key_ready, recipient_x25519_pub, tee_bls_pub),
            Ok(other) => return Err(eyre::eyre!("expected enclave PublicKeys, got {other:?}")),
            Err(e) => return Err(eyre::eyre!("enclave GetPublicKeys failed: {e}")),
        };
    println!(
        "enclave offer pubkey (recipient_x25519): 0x{}",
        hex::encode(offer_pub)
    );
    println!(
        "enclave tee_bls_pub (DKG identity):      0x{}",
        hex::encode(&tee_bls_pub)
    );
    println!("permanent offer key ready:                 {offer_key_ready}");
    println!("attestation:                             {label}");
    println!("hardware quote type reported:           {hardware_quote_type_reported}");
    println!(
        "mrenclave:                               0x{}",
        hex::encode(mrenclave)
    );
    println!(
        "mrsigner:                                0x{}",
        hex::encode(mrsigner)
    );
    println!("isv_svn:                                 {isv_svn}");
    if !diff_chain {
        return Ok(());
    }
    if !offer_key_ready {
        return Err(eyre::eyre!(
            "enclave permanent offer key is not ready; onboarding recipient cannot be compared with chain state"
        ));
    }

    let onchain = call_u256(
        client,
        ITeeRegistry::tributeOfferPublicKeyCall {}.abi_encode(),
    )
    .await?;
    if onchain.is_zero() {
        return Err(eyre::eyre!(
            "on-chain tributeOfferPublicKey == 0 - chain is not TEE-bootstrapped yet, \
             nothing to diff against"
        ));
    }
    let onchain_bytes: [u8; 32] = onchain.to_be_bytes();
    println!(
        "on-chain tributeOfferPublicKey (slot-1): 0x{}",
        hex::encode(onchain_bytes)
    );
    if onchain_bytes == offer_pub {
        println!("[OK] MATCH - enclave resident offer key == on-chain registry");
        Ok(())
    } else {
        Err(eyre::eyre!(
            "[FAIL] MISMATCH - enclave offer key 0x{} != on-chain 0x{}",
            hex::encode(offer_pub),
            hex::encode(onchain_bytes)
        ))
    }
}

pub(super) fn authorize_validator_node_binding(
    chain_id: [u8; 32],
    genesis_hash: B256,
    node_id_hash: B256,
    evm_signer: &crate::tx::TxSigner,
) -> Result<(ValidatorNodeBindingV1, B256, [u8; 65])> {
    let binding = ValidatorNodeBindingV1 {
        chain_id,
        genesis_hash,
        validator: evm_signer.address().into_array(),
        node_id_hash,
    };
    let binding_hash = binding
        .binding_hash()
        .map_err(|error| eyre::eyre!("hash address-to-NodeHost binding: {error}"))?;
    let signature = sign_node_hash(evm_signer.key(), binding_hash)
        .map_err(|error| eyre::eyre!("sign address-to-NodeHost binding: {error}"))?;
    Ok((binding, binding_hash, signature))
}

pub(super) fn parse_nonzero_b256(value: &str, argument: &'static str) -> Result<B256> {
    let value = B256::from(parse_hex_array::<32>(value, argument)?);
    if value.is_zero() {
        return Err(eyre::eyre!("{argument} must be nonzero"));
    }
    Ok(value)
}

fn parse_hex_array<const N: usize>(value: &str, argument: &'static str) -> Result<[u8; N]> {
    let encoded = value.strip_prefix("0x").unwrap_or(value);
    let decoded = hex::decode(encoded).wrap_err_with(|| format!("decode {argument} as hex"))?;
    decoded.try_into().map_err(|decoded: Vec<u8>| {
        eyre::eyre!(
            "{argument} must contain exactly {N} bytes, got {}",
            decoded.len()
        )
    })
}

pub(super) fn load_secp256k1_key_file(path: &std::path::Path) -> Result<k256::ecdsa::SigningKey> {
    let metadata =
        fs::metadata(path).wrap_err_with(|| format!("stat Reth P2P secret {}", path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 130 {
        return Err(eyre::eyre!(
            "Reth P2P secret {} must be a bounded regular file",
            path.display()
        ));
    }
    let encoded = Zeroizing::new(
        fs::read(path).wrap_err_with(|| format!("read Reth P2P secret {}", path.display()))?,
    );
    parse_secp256k1_key_bytes(encoded.as_ref())
}

pub(super) fn compressed_public_key(key: &k256::ecdsa::SigningKey) -> Result<[u8; 33]> {
    key.verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .map_err(|_| eyre::eyre!("Reth P2P public key is not compressed SEC1-33"))
}

pub(super) fn ensure_signer_matches_node_id(
    key: &k256::ecdsa::SigningKey,
    node_id: &NodeIdV1,
) -> Result<()> {
    if compressed_public_key(key)? != node_id.reth_p2p_public {
        eyre::bail!("Reth P2P secret does not match the committed NodeHost identity");
    }
    Ok(())
}

fn parse_secp256k1_key_bytes(encoded: &[u8]) -> Result<k256::ecdsa::SigningKey> {
    let secret = if encoded.len() == 32 {
        let mut secret = Zeroizing::new([0_u8; 32]);
        secret.copy_from_slice(encoded);
        secret
    } else {
        let text = std::str::from_utf8(encoded)
            .wrap_err("Reth P2P secret is neither raw bytes nor UTF-8 hex")?;
        let text = text.trim();
        let text = text.strip_prefix("0x").unwrap_or(text);
        let decoded = Zeroizing::new(hex::decode(text).wrap_err("decode Reth P2P secret as hex")?);
        if decoded.len() != 32 {
            return Err(eyre::eyre!(
                "Reth P2P secret must contain exactly 32 bytes, got {}",
                decoded.len()
            ));
        }
        let mut secret = Zeroizing::new([0_u8; 32]);
        secret.copy_from_slice(decoded.as_ref());
        secret
    };
    k256::ecdsa::SigningKey::from_bytes((&*secret).into())
        .map_err(|error| eyre::eyre!("invalid Reth P2P secp256k1 secret: {error}"))
}

pub(super) fn sign_node_hash(
    key: &k256::ecdsa::SigningKey,
    hash: B256,
) -> std::result::Result<[u8; 65], String> {
    let (signature, recovery_id): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = key
        .sign_prehash(hash.as_slice())
        .map_err(|error| format!("secp256k1 signing failed: {error}"))?;
    let mut encoded = [0_u8; 65];
    encoded[..64].copy_from_slice(&signature.to_bytes());
    encoded[64] = recovery_id.to_byte();
    Ok(encoded)
}

pub(super) fn development_identity_v1(
    policy: &TeePolicyV1,
    node_id: &NodeIdV1,
    recipient_x25519: [u8; 32],
    attestation_ed25519: [u8; 32],
    noise_responder_x25519: [u8; 32],
) -> Result<(B256, B256)> {
    let mut enclave_keys = [0_u8; 96];
    enclave_keys[..32].copy_from_slice(&recipient_x25519);
    enclave_keys[32..64].copy_from_slice(&attestation_ed25519);
    enclave_keys[64..].copy_from_slice(&noise_responder_x25519);

    let mut enclave_preimage = Vec::with_capacity(ENCLAVE_ID_DOMAIN_V1.len() + enclave_keys.len());
    enclave_preimage.extend_from_slice(ENCLAVE_ID_DOMAIN_V1);
    enclave_preimage.extend_from_slice(&enclave_keys);
    let enclave_id = keccak256(enclave_preimage);

    let node_id_hash = node_id
        .node_id_hash()
        .map_err(|error| eyre::eyre!("invalid development node identity: {error}"))?;
    let mut authorization_preimage =
        Vec::with_capacity(DEV_NODE_HOST_DOMAIN_V1.len() + 32 + 32 + 32 + enclave_keys.len());
    authorization_preimage.extend_from_slice(DEV_NODE_HOST_DOMAIN_V1);
    authorization_preimage.extend_from_slice(&policy.chain_id);
    authorization_preimage.extend_from_slice(policy.genesis_hash.as_slice());
    authorization_preimage.extend_from_slice(node_id_hash.as_slice());
    authorization_preimage.extend_from_slice(&enclave_keys);
    Ok((enclave_id, keccak256(authorization_preimage)))
}
